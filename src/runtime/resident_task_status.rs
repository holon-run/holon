//! Read-only TaskStatus projections bound to the selected exact Task version.
use super::*;
use crate::types::ChildSupervisionProjection;
use serde_json::Value;

pub(crate) const TASK_WORKTREE_CLEANUP_FIELDS: [&str; 9] = [
    "cleanup_status",
    "cleanup_reason",
    "cleanup_error",
    "auto_cleaned_up",
    "changed_files",
    "retained_for_review",
    "actual_branch",
    "branch_cleanup_status",
    "branch_cleanup_error",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub(crate) enum TaskWorktreeObservation {
    #[default]
    Unavailable,
    Observed {
        fields: BTreeMap<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskDelegationMetadata {
    pub child_agent_id: String,
    pub parent_work_item_id: String,
    pub child_work_item_id: String,
    pub delegation_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub(crate) enum TaskDelegationObservation {
    #[default]
    Unavailable,
    Observed {
        latest: Option<TaskDelegationMetadata>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskStatusProjectionObservation {
    pub task: TaskFactLocator,
    pub worktree: TaskWorktreeObservation,
    pub delegation: TaskDelegationObservation,
}

impl TaskStatusProjectionObservation {
    /// Count borrowed JSON before any owned status projection is constructed.
    pub(crate) fn projection_material_nodes(&self, record: &QualifiedTaskFact) -> Result<usize> {
        let CanonicalTaskFact::Task(task) = &record.fact else {
            anyhow::bail!("TaskStatus material has another family");
        };
        self.json_node_count()?
            .checked_add(
                task.detail
                    .as_ref()
                    .map_or(Ok(0), |value| value_node_count(value, 0))?,
            )
            .and_then(|nodes| nodes.checked_add(32))
            .context("TaskStatus projection material count overflow")
    }

    fn validate(&self, record: &QualifiedTaskFact) -> Result<()> {
        record.validate(&self.task.scope)?;
        ensure!(
            self.task == record.locator,
            "TaskStatus projection has another Task version"
        );
        let CanonicalTaskFact::Task(task) = &record.fact else {
            anyhow::bail!("TaskStatus projection has another family");
        };
        if let TaskWorktreeObservation::Observed { fields } = &self.worktree {
            ensure!(
                fields
                    .keys()
                    .all(|key| TASK_WORKTREE_CLEANUP_FIELDS.contains(&key.as_str())),
                "TaskStatus projection includes a non-cleanup worktree field"
            );
            if !fields.is_empty() {
                ensure!(
                    matches!(
                        task.status,
                        TaskStatus::Completed
                            | TaskStatus::Failed
                            | TaskStatus::Cancelled
                            | TaskStatus::Interrupted
                    ),
                    "active Task cannot expose terminal cleanup evidence"
                );
                ensure!(
                    task.detail
                        .as_ref()
                        .and_then(|detail| detail.get("worktree"))
                        .is_some_and(Value::is_object),
                    "TaskStatus cleanup lacks the original artifact"
                );
            }
        }
        if let TaskDelegationObservation::Observed {
            latest: Some(latest),
        } = &self.delegation
        {
            let child = ChildSupervisionProjection::from_task_record(task)
                .context("TaskStatus delegation lacks its original child association")?;
            ensure!(
                latest.child_agent_id == child.child_agent_id,
                "TaskStatus delegation differs from its selected child"
            );
        }
        Ok(())
    }

    /// The caller already holds/accounted its selected Task material.
    pub(crate) fn overlay(&self, record: &QualifiedTaskFact) -> Result<TaskRecord> {
        self.validate(record)?;
        let TaskWorktreeObservation::Observed { fields } = &self.worktree else {
            return Err(TaskReadError::IncompleteStatusProjection.into());
        };
        let CanonicalTaskFact::Task(task) = &record.fact else {
            unreachable!()
        };
        let mut observed = task.clone();
        if !fields.is_empty() {
            let worktree = observed
                .detail
                .as_mut()
                .and_then(|detail| detail.get_mut("worktree"))
                .and_then(Value::as_object_mut)
                .context("original worktree disappeared")?;
            for (name, value) in fields {
                worktree.insert(name.clone(), value.clone());
            }
        }
        Ok(observed)
    }

    pub(crate) fn child_supervision(
        &self,
        record: &QualifiedTaskFact,
    ) -> Result<Option<ChildSupervisionProjection>> {
        let task = self.overlay(record)?;
        let Some(mut child) = ChildSupervisionProjection::from_task_record(&task) else {
            return Ok(None);
        };
        // The original delegation lookup was optional. Unavailable remains
        // explicit metadata, without fabricating an observed absent relation.
        if let TaskDelegationObservation::Observed {
            latest: Some(latest),
        } = &self.delegation
        {
            child.parent_work_item_id = Some(latest.parent_work_item_id.clone());
            child.child_work_item_id = Some(latest.child_work_item_id.clone());
            child.delegation_id = Some(latest.delegation_id.clone());
        }
        Ok(Some(child))
    }

    fn json_node_count(&self) -> Result<usize> {
        let worktree = match &self.worktree {
            TaskWorktreeObservation::Unavailable => 2,
            TaskWorktreeObservation::Observed { fields } => {
                fields.values().try_fold(3usize, |count, value| {
                    count
                        .checked_add(value_node_count(value, 0)?)
                        .context("TaskStatus metadata count overflow")
                })?
            }
        };
        let delegation = match &self.delegation {
            TaskDelegationObservation::Unavailable => 2,
            TaskDelegationObservation::Observed { latest: None } => 3,
            TaskDelegationObservation::Observed { latest: Some(_) } => 7,
        };
        // Object + exact Task locator's ten JSON nodes, then both observations.
        11usize
            .checked_add(worktree)
            .and_then(|n| n.checked_add(delegation))
            .context("TaskStatus metadata count overflow")
    }
}

fn value_node_count(value: &Value, depth: usize) -> Result<usize> {
    ensure!(
        depth <= 128,
        TaskReadError::ResourceExhausted("TaskStatus metadata nesting")
    );
    match value {
        Value::Array(values) => values.iter().try_fold(1usize, |count, value| {
            count
                .checked_add(value_node_count(value, depth + 1)?)
                .context("TaskStatus metadata count overflow")
        }),
        Value::Object(values) => values.values().try_fold(1usize, |count, value| {
            count
                .checked_add(value_node_count(value, depth + 1)?)
                .context("TaskStatus metadata count overflow")
        }),
        _ => Ok(1),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskStatusProjectionCoverage {
    pub heads: BTreeMap<String, TaskStatusProjectionObservation>,
}
impl TaskStatusProjectionCoverage {
    pub(super) fn validate(
        &self,
        scope: &InputEvidenceScope,
        records: &BTreeMap<TaskKey, Arc<QualifiedTaskFact>>,
    ) -> Result<()> {
        for (id, head) in &self.heads {
            let record = records
                .get(&TaskKey::new(TaskFamily::Task, id))
                .ok_or_else(|| {
                    TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::Task, id))
                })?;
            ensure!(
                head.task.scope == *scope,
                "TaskStatus projection crossed scope"
            );
            head.validate(record)?;
        }
        Ok(())
    }
    pub(crate) fn owned_metadata_record_count(&self) -> Result<usize> {
        if self.heads.is_empty() {
            return Ok(0);
        }
        self.heads.values().try_fold(2usize, |count, head| {
            count
                .checked_add(head.json_node_count()?)
                .context("TaskStatus metadata count overflow")
        })
    }
}

impl CapturedTasks {
    pub(crate) fn task_status_projection(
        &self,
        task_id: &str,
    ) -> Result<&TaskStatusProjectionObservation> {
        let key = TaskKey::new(TaskFamily::Task, task_id);
        let record = self
            .lookup(&key)?
            .ok_or_else(|| TaskReadError::NeedsPreparation(key))?;
        let head = self
            .coverage
            .status
            .heads
            .get(task_id)
            .ok_or(TaskReadError::IncompleteStatusProjection)?;
        head.validate(record)?;
        Ok(head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TaskKind;
    use chrono::Utc;
    fn fact() -> QualifiedTaskFact {
        let at = "2026-10-13T00:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let task = TaskRecord {
            id: "status-task".into(),
            agent_id: "status-parent".into(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Cancelled,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            recovery: None,
            detail: Some(
                serde_json::json!({"child_agent_id":"original-child", "worktree":{"path":"/original", "branch":"original"}}),
            ),
        };
        let fact = CanonicalTaskFact::Task(task);
        QualifiedTaskFact {
            locator: TaskFactLocator {
                scope: InputEvidenceScope {
                    agent_id: "status-parent".into(),
                    identity_incarnation: 1,
                },
                key: TaskKey::new(TaskFamily::Task, "status-task"),
                revision: 1,
                content_digest: "a".repeat(64),
                projection_digest: "b".repeat(64),
            },
            resident_bytes: serde_json::to_vec(&fact).unwrap().len(),
            fact,
        }
    }
    fn projection(record: &QualifiedTaskFact) -> TaskStatusProjectionObservation {
        TaskStatusProjectionObservation {
            task: record.locator.clone(),
            worktree: TaskWorktreeObservation::Observed {
                fields: BTreeMap::from([
                    ("cleanup_status".into(), serde_json::json!("retained")),
                    ("changed_files".into(), serde_json::json!(["a", "b"])),
                ]),
            },
            delegation: TaskDelegationObservation::Observed {
                latest: Some(TaskDelegationMetadata {
                    child_agent_id: "original-child".into(),
                    parent_work_item_id: "parent-work".into(),
                    child_work_item_id: "child-work".into(),
                    delegation_id: "original-delegation".into(),
                }),
            },
        }
    }
    #[test]
    fn exact_projection_keeps_artifact_and_exposes_original_supervision() -> Result<()> {
        let record = fact();
        let head = projection(&record);
        let task = head.overlay(&record)?;
        assert_eq!(
            task.detail.as_ref().unwrap()["worktree"]["path"],
            "/original"
        );
        assert_eq!(
            task.detail.as_ref().unwrap()["worktree"]["cleanup_status"],
            "retained"
        );
        let child = head.child_supervision(&record)?.unwrap();
        assert_eq!(child.delegation_id.as_deref(), Some("original-delegation"));
        assert_eq!(child.parent_work_item_id.as_deref(), Some("parent-work"));
        let mut later = record.clone();
        later.locator.revision += 1;
        assert!(head.overlay(&later).is_err());
        let mut wrong = head.clone();
        wrong.task.scope.identity_incarnation += 1;
        assert!(wrong.overlay(&record).is_err());
        Ok(())
    }
    #[test]
    fn invalid_projection_never_replaces_artifact_or_foreign_child() {
        let record = fact();
        let mut head = projection(&record);
        let TaskWorktreeObservation::Observed { fields } = &mut head.worktree else {
            unreachable!()
        };
        fields.insert("path".into(), serde_json::json!("/foreign"));
        assert!(head.overlay(&record).is_err());
        let mut head = projection(&record);
        let TaskDelegationObservation::Observed {
            latest: Some(latest),
        } = &mut head.delegation
        else {
            unreachable!()
        };
        latest.child_agent_id = "foreign-child".into();
        assert!(head.child_supervision(&record).is_err());
    }
    #[test]
    fn absent_and_unavailable_are_distinct_and_missing_cleanup_blocks_overlay() -> Result<()> {
        let record = fact();
        let mut head = projection(&record);
        head.delegation = TaskDelegationObservation::Unavailable;
        assert!(head
            .child_supervision(&record)?
            .unwrap()
            .delegation_id
            .is_none());
        let unavailable = serde_json::to_value(&head.delegation)?;
        head.delegation = TaskDelegationObservation::Observed { latest: None };
        assert_ne!(unavailable, serde_json::to_value(&head.delegation)?);
        head.worktree = TaskWorktreeObservation::Unavailable;
        assert!(matches!(
            head.overlay(&record)
                .unwrap_err()
                .downcast_ref::<TaskReadError>(),
            Some(TaskReadError::IncompleteStatusProjection)
        ));
        Ok(())
    }
    #[test]
    fn scalar_json_nodes_match_sql_before_owned_metadata_decode() -> Result<()> {
        let connection = rusqlite::Connection::open_in_memory()?;
        let record = fact();
        for worktree in [
            TaskWorktreeObservation::Unavailable,
            TaskWorktreeObservation::Observed {
                fields: BTreeMap::new(),
            },
            projection(&record).worktree,
        ] {
            for delegation in [
                TaskDelegationObservation::Unavailable,
                TaskDelegationObservation::Observed { latest: None },
                projection(&record).delegation,
            ] {
                let coverage = TaskStatusProjectionCoverage {
                    heads: BTreeMap::from([(
                        record.locator.key.id.clone(),
                        TaskStatusProjectionObservation {
                            task: record.locator.clone(),
                            worktree: worktree.clone(),
                            delegation,
                        },
                    )]),
                };
                let raw = serde_json::to_string(&coverage)?;
                let sql: usize =
                    connection.query_row("SELECT COUNT(*) FROM json_tree(?1)", [raw], |row| {
                        row.get(0)
                    })?;
                assert_eq!(coverage.owned_metadata_record_count()?, sql);
            }
        }
        Ok(())
    }
}
