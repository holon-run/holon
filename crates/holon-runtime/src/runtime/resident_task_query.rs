//! Task output selection metadata, independent from Task parent-message lineage.
use super::*;
use crate::runtime::resident_input_evidence::{EvidenceLocator, InputEvidenceFamily};

pub(crate) const TASK_OUTPUT_RECENT_MESSAGES: usize = 200;

/// The original TaskOutput status override depends only on received metadata.
pub(crate) fn task_output_status_from_message(
    message: &crate::types::MessageEnvelope,
    metadata: &serde_json::Value,
) -> crate::types::TaskStatus {
    use crate::types::{MessageKind, TaskStatus};
    match metadata
        .get("task_status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(match message.kind {
            MessageKind::TaskStatus => "running",
            MessageKind::TaskResult => "completed",
            _ => "queued",
        }) {
        "running" => TaskStatus::Running,
        "cancelling" => TaskStatus::Cancelling,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "interrupted" => TaskStatus::Interrupted,
        _ => TaskStatus::Queued,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskOutputSourcePlace {
    RecentResult,
    RecentOther,
    HistoricalResult,
    HistoricalOther,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskOutputSourceAvailability {
    #[default]
    Present,
    Pruned,
    MissingVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskOutputSourceHead {
    pub place: TaskOutputSourcePlace,
    pub message: EvidenceLocator,
    pub message_status: crate::types::TaskStatus,
    #[serde(default)]
    pub availability: TaskOutputSourceAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskOutputHeadObservation {
    pub task: TaskFactLocator,
    pub source: Option<TaskOutputSourceHead>,
}
impl TaskOutputHeadObservation {
    pub(crate) fn validate(&self, task: &TaskFactLocator) -> Result<()> {
        ensure!(
            self.task == *task,
            "Task output head has another Task version"
        );
        if let Some(head) = &self.source {
            ensure!(
                head.message.scope == task.scope
                    && head.message.key.family == InputEvidenceFamily::Message
                    && head.message.revision > 0
                    && valid_digest(&head.message.content_digest),
                "Task output source locator differs from its namespace"
            );
        }
        Ok(())
    }
}

/// Exact global window membership. Bodies belong to the selected Input capture.
/// One copy is shared by all heads in a Task coverage, never one per Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskOutputWindowObservation {
    /// Original recent reader order: newest first, message_id ASC on ties.
    pub message_ids: Vec<String>,
    pub is_exhaustive: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskOutputCoverage {
    pub window: Option<TaskOutputWindowObservation>,
    pub heads: BTreeMap<String, TaskOutputHeadObservation>,
}
impl TaskOutputCoverage {
    pub(crate) fn head(&self, task: &TaskFactLocator) -> Result<Option<&TaskOutputSourceHead>> {
        let window = self
            .window
            .as_ref()
            .ok_or(TaskReadError::IncompleteOutput)?;
        ensure!(
            window.message_ids.len() <= TASK_OUTPUT_RECENT_MESSAGES,
            "Task output window exceeds original policy"
        );
        let head = self
            .heads
            .get(&task.key.id)
            .ok_or_else(|| TaskReadError::NeedsPreparation(task.key.clone()))?;
        head.validate(task)?;
        if let Some(source) = &head.source {
            if source.availability != TaskOutputSourceAvailability::Present {
                return Err(TaskReadError::UnavailableOutputSource(source.availability).into());
            }
        }
        Ok(head.source.as_ref())
    }
}

impl TaskOutputCoverage {
    pub(super) fn validate(
        &self,
        scope: &InputEvidenceScope,
        records: &BTreeMap<TaskKey, Arc<QualifiedTaskFact>>,
    ) -> Result<()> {
        if self.window.is_none() {
            ensure!(
                self.heads.is_empty(),
                "Task output heads lack a complete window"
            );
            return Ok(());
        }
        let window = self.window.as_ref().expect("window observed");
        ensure!(
            window.message_ids.len() <= TASK_OUTPUT_RECENT_MESSAGES
                && window.message_ids.iter().collect::<BTreeSet<_>>().len()
                    == window.message_ids.len(),
            "Task output window is invalid"
        );
        for (id, head) in &self.heads {
            let record = records
                .get(&TaskKey::new(TaskFamily::Task, id))
                .ok_or_else(|| {
                    TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::Task, id))
                })?;
            ensure!(record.locator.scope == *scope, "Task output scope changed");
            head.validate(&record.locator)?;
            if let Some(source) = &head.source {
                let in_window = window.message_ids.contains(&source.message.key.id);
                ensure!(
                    in_window
                        == matches!(
                            source.place,
                            TaskOutputSourcePlace::RecentResult
                                | TaskOutputSourcePlace::RecentOther
                        ),
                    "Task output place differs from its exact window"
                );
            }
        }
        Ok(())
    }
}
