//! Complete query relations are observations; live leases choose their use.
use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskQueryCoverage {
    /// None means original Task history was unavailable, never an empty baseline.
    pub birth_cursor: Option<u64>,
    pub birth_after: Option<u64>,
    pub born_task_seqs: BTreeMap<String, u64>,
    /// An explicit empty set is a complete same-view reply observation.
    pub reply_task_ids: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub parent_turn_tasks: BTreeMap<String, BTreeMap<String, TaskParentTurnHeader>>,
}
/// Accepted status metadata only; this locator never grants Task/Input bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskParentTurnHeader {
    pub task: TaskFactLocator,
    pub parent_turn_id: String,
    pub status: crate::types::TaskStatus,
    pub availability: TaskAvailability,
}
impl TaskQueryCoverage {
    pub(crate) fn all_selected_task_ids(&self) -> impl Iterator<Item = &str> {
        self.born_task_seqs.keys().map(String::as_str).chain(
            self.reply_task_ids
                .values()
                .flat_map(|ids| ids.iter().map(String::as_str)),
        )
    }
    pub(crate) fn owned_metadata_record_count(&self) -> Result<usize> {
        let relation_records =
            self.parent_turn_tasks
                .values()
                .try_fold(self.parent_turn_tasks.len(), |n, rows| {
                    n.checked_add(
                        rows.len()
                            .checked_mul(14)
                            .context("parent Turn Task header count overflow")?,
                    )
                    .context("parent Turn Task header count overflow")
                })?;
        self.reply_task_ids.values().try_fold(
            self.born_task_seqs
                .len()
                .checked_add(self.reply_task_ids.len())
                .and_then(|n| n.checked_add(relation_records))
                .context("Task query metadata count overflow")?,
            |n, ids| {
                n.checked_add(ids.len())
                    .context("Task query metadata count overflow")
            },
        )
    }
    pub(super) fn validate(&self, coverage: &TaskCoverage) -> Result<()> {
        if let Some(after) = self.birth_after {
            let cursor = self.birth_cursor.ok_or(TaskReadError::IncompleteQuery)?;
            ensure!(
                after <= cursor,
                "Task query cursor exceeds accepted high water"
            );
            ensure!(
                self.born_task_seqs
                    .values()
                    .all(|seq| *seq > after && *seq <= cursor),
                "Task query birth lies outside its observed interval"
            );
        } else {
            ensure!(
                self.born_task_seqs.is_empty(),
                "Task query births lack a complete interval"
            );
        }
        for (turn, headers) in &self.parent_turn_tasks {
            ensure!(!turn.is_empty(), "parent Turn Task query has an empty Turn");
            for (id, header) in headers {
                ensure!(
                    header.parent_turn_id == *turn
                        && header.task.key == TaskKey::new(TaskFamily::Task, id)
                        && header.task.revision > 0
                        && header.task.revision <= coverage.revision
                        && valid_digest(&header.task.content_digest)
                        && valid_digest(&header.task.projection_digest),
                    "parent Turn Task header differs"
                );
            }
        }
        for id in self.all_selected_task_ids() {
            ensure!(
                matches!(
                    coverage
                        .observations
                        .get(&TaskKey::new(TaskFamily::Task, id)),
                    Some(TaskAvailability::Present | TaskAvailability::Pruned)
                ),
                "Task query relation lacks an accepted availability observation"
            );
        }
        Ok(())
    }
    fn selected_keys(&self, query: &TaskQuerySelection) -> Result<BTreeSet<TaskKey>> {
        let mut keys = query.required_keys.clone();
        if let Some(after) = query.birth_after {
            ensure!(
                self.birth_cursor.is_some_and(|cursor| after <= cursor)
                    && (self.birth_cursor == Some(after)
                        || self.birth_after.is_some_and(|observed| observed <= after)),
                TaskReadError::IncompleteQuery
            );
            keys.extend(
                self.born_task_seqs
                    .iter()
                    .filter(|(_, seq)| **seq > after)
                    .map(|(id, _)| TaskKey::new(TaskFamily::Task, id)),
            );
        }
        for message in &query.reply_message_ids {
            let ids = self
                .reply_task_ids
                .get(message)
                .ok_or(TaskReadError::IncompleteQuery)?;
            keys.extend(ids.iter().map(|id| TaskKey::new(TaskFamily::Task, id)));
        }
        for turn in &query.parent_turn_ids {
            ensure!(
                self.parent_turn_tasks.contains_key(turn),
                TaskReadError::IncompleteQuery
            );
        }
        Ok(keys)
    }
}

impl ResidentTaskDomain {
    pub(crate) fn capture_query(&self, query: &TaskQuerySelection) -> Result<CapturedTasks> {
        let count = self
            .coverage
            .query
            .owned_metadata_record_count()?
            .checked_add(query.required_keys.len())
            .context("Task query capture count overflow")?;
        let bytes = serialized_bytes(&self.coverage.query)?
            .checked_add(serialized_bytes(query)?)
            .and_then(|n| n.checked_add(u64::try_from(count).ok()?.checked_mul(256)?))
            .and_then(|n| n.checked_mul(4))
            .context("Task query capture bytes overflow")?;
        ensure!(
            bytes <= u64::try_from(self.byte_limit)?
                && count.checked_mul(4).is_some_and(|n| n <= self.record_limit),
            TaskReadError::ResourceExhausted("Task query capture selectors")
        );
        let selector = self
            .budget
            .as_ref()
            .map(|ledger| {
                ledger.register_owned_bytes(
                    &self.scope,
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: bytes,
                    },
                )
            })
            .transpose()?;
        let request = TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: self.coverage.query.selected_keys(query)?,
        };
        let mut captured = self.capture(&request)?;
        captured._budget_views.extend(selector);
        Ok(captured)
    }
}
impl CapturedTasks {
    pub(crate) fn parent_turn_task_headers(
        &self,
        turn: &str,
    ) -> Result<&BTreeMap<String, TaskParentTurnHeader>> {
        let headers = self
            .coverage
            .query
            .parent_turn_tasks
            .get(turn)
            .ok_or(TaskReadError::IncompleteQuery)?;
        for (id, header) in headers {
            ensure!(
                header.task.scope == self.scope
                    && header.parent_turn_id == turn
                    && header.task.key == TaskKey::new(TaskFamily::Task, id),
                "parent Turn query scope/key differs"
            );
            ensure!(
                header.availability == TaskAvailability::Present,
                TaskReadError::UnavailableQuery(header.task.key.clone())
            );
        }
        Ok(headers)
    }
    #[cfg(test)]
    pub(crate) fn task_birth_cursor(&self) -> Result<u64> {
        self.coverage
            .query
            .birth_cursor
            .ok_or_else(|| TaskReadError::IncompleteQuery.into())
    }
    pub(crate) fn born_task_ids_after(&self, after: u64) -> Result<impl Iterator<Item = &str>> {
        ensure!(
            self.coverage
                .query
                .birth_cursor
                .is_some_and(|cursor| after <= cursor)
                && (self.coverage.query.birth_cursor == Some(after)
                    || self
                        .coverage
                        .query
                        .birth_after
                        .is_some_and(|observed| observed <= after)),
            TaskReadError::IncompleteQuery
        );
        for (id, seq) in &self.coverage.query.born_task_seqs {
            if *seq > after {
                self.require_query_task(id)?;
            }
        }
        Ok(self
            .coverage
            .query
            .born_task_seqs
            .iter()
            .filter(move |(_, seq)| **seq > after)
            .map(|(id, _)| id.as_str()))
    }
    pub(crate) fn reply_task_ids(&self, message: &str) -> Result<&BTreeSet<String>> {
        let ids = self
            .coverage
            .query
            .reply_task_ids
            .get(message)
            .ok_or(TaskReadError::IncompleteQuery)?;
        for id in ids {
            let task = self.require_query_task(id)?;
            ensure!(
                task.kind == crate::types::TaskKind::AgentMessageWait
                    && task.status == TaskStatus::Completed
                    && task
                        .detail
                        .as_ref()
                        .and_then(|detail| detail.get("message_id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(message),
                "accepted reply relation differs from qualified Task body"
            );
        }
        Ok(ids)
    }
    fn require_query_task(&self, id: &str) -> Result<&TaskRecord> {
        let key = TaskKey::new(TaskFamily::Task, id);
        ensure!(
            self.coverage.observations.get(&key) == Some(&TaskAvailability::Present),
            TaskReadError::UnavailableQuery(key.clone())
        );
        let fact = self
            .lookup(&key)?
            .ok_or_else(|| TaskReadError::UnavailableQuery(key))?;
        let CanonicalTaskFact::Task(task) = &fact.fact else {
            anyhow::bail!("Task query family differs")
        };
        Ok(task)
    }
}
