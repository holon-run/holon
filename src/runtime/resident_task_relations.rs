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
    fn validate_selection(&self, query: &TaskQuerySelection) -> Result<()> {
        if let Some(after) = query.birth_after {
            ensure!(
                self.birth_cursor.is_some_and(|cursor| after <= cursor)
                    && (self.birth_cursor == Some(after)
                        || self.birth_after.is_some_and(|observed| observed <= after)),
                TaskReadError::IncompleteQuery
            );
        }
        for message in &query.reply_message_ids {
            ensure!(
                self.reply_task_ids.contains_key(message),
                TaskReadError::IncompleteQuery
            );
        }
        for turn in &query.parent_turn_ids {
            ensure!(
                self.parent_turn_tasks.contains_key(turn),
                TaskReadError::IncompleteQuery
            );
        }
        Ok(())
    }
    fn selected_keys(&self, query: &TaskQuerySelection) -> Result<BTreeSet<TaskKey>> {
        self.validate_selection(query)?;
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
    // Only the original Task <-> result relation is traversed. These borrowed
    // predicates allocate no key, selected set, coverage or body Arc.
    fn query_root_contains(&self, query: &TaskQuerySelection, key: &TaskKey) -> bool {
        query.required_keys.contains(key)
            || (key.family == TaskFamily::Task
                && (query.birth_after.is_some_and(|after| {
                    self.coverage
                        .query
                        .born_task_seqs
                        .get(&key.id)
                        .is_some_and(|seq| *seq > after)
                }) || query.reply_message_ids.iter().any(|message| {
                    self.coverage
                        .query
                        .reply_task_ids
                        .get(message)
                        .is_some_and(|ids| ids.contains(&key.id))
                })))
    }
    fn query_selects_task(&self, query: &TaskQuerySelection, key: &TaskKey) -> bool {
        self.query_root_contains(query, key)
            || query.required_keys.iter().any(|result| {
                result.family == TaskFamily::ResultSettlement
                    && self.coverage.observations.get(result) == Some(&TaskAvailability::Present)
                    && self.coverage.result_dependencies.get(&result.id).is_some_and(|dependency| {
                        matches!(&dependency.relation, TaskResultDependencyRelation::QualifiedCurrent { task }
                            if task.key == *key)
                    })
            })
    }
    fn query_selects_key(&self, query: &TaskQuerySelection, key: &TaskKey) -> bool {
        if key.family == TaskFamily::Task {
            return self.query_selects_task(query, key);
        }
        if self.query_root_contains(query, key) {
            return true;
        }
        if self.coverage.observations.get(key) == Some(&TaskAvailability::Present) {
            if let Some(TaskResultDependencyObservation {
                relation: TaskResultDependencyRelation::QualifiedCurrent { task },
                ..
            }) = self.coverage.result_dependencies.get(&key.id)
            {
                return self.coverage.observations.get(&task.key)
                    == Some(&TaskAvailability::Present)
                    && self.query_selects_task(query, &task.key)
                    && self
                        .coverage
                        .task_result_ids
                        .get(&task.key.id)
                        .is_some_and(|ids| ids.contains(&key.id));
            }
        }
        // Pruned related keys still occupy closure scratch, even when they no
        // longer have a dependency/body. Invalid results grant no Task edge.
        self.coverage.task_result_ids.iter().any(|(task_id, ids)| {
            ids.contains(&key.id)
                && self.records.keys().any(|task| {
                    task.family == TaskFamily::Task
                        && &task.id == task_id
                        && self.query_selects_task(query, task)
                })
        })
    }
    fn validate_query_reverse_relations(&self) -> Result<()> {
        for (task, ids) in &self.coverage.task_result_ids {
            for id in ids {
                let Some(meta) = self.coverage.result_metadata.get(id) else {
                    continue;
                };
                // Actual Task/Result writers derive this inverse from task_id.
                // A damaged extra edge must not defeat the borrowed two-hop
                // prediction before the original closure executor runs.
                ensure!(meta.task_id == *task, "Task query reverse relation changed");
            }
        }
        Ok(())
    }
    /// Includes the actual full coverage copy, but only selected physical
    /// bodies. Caller material is a loan, never another ledger registration.
    pub(crate) fn capture_query_with_prior(
        &self,
        query: &TaskQuerySelection,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<(CapturedTasks, (u64, usize))> {
        self.coverage.query.validate_selection(query)?;
        self.validate_query_reverse_relations()?;
        let selector_count = self
            .coverage
            .query
            .owned_metadata_record_count()?
            .checked_add(query.required_keys.len())
            .context("Task query capture count overflow")?;
        let selector_bytes = serialized_bytes(&self.coverage.query)?
            .checked_add(serialized_bytes(query)?)
            .and_then(|n| n.checked_add(u64::try_from(selector_count).ok()?.checked_mul(256)?))
            .and_then(|n| n.checked_mul(4))
            .context("Task query capture bytes overflow")?;
        let (_, coverage_nodes) =
            super::super::input_evidence_budget::borrowed_json_material(&self.coverage)?;
        let mut key_bytes = 0u64;
        let mut key_count = 0usize;
        for key in self
            .coverage
            .observations
            .keys()
            .filter(|key| self.query_selects_key(query, key))
        {
            key_bytes = key_bytes
                .checked_add(serialized_bytes(key)?)
                .context("Task query closure bytes overflow")?;
            key_count = key_count
                .checked_add(1)
                .context("Task query closure count overflow")?;
        }
        let extra_records = coverage_nodes
            .checked_add(
                key_count
                    .checked_mul(4)
                    .context("Task query closure nodes overflow")?,
            )
            .and_then(|n| n.checked_add(4))
            .context("Task query copy nodes overflow")?;
        let extra_bytes = key_bytes
            .checked_mul(4)
            .and_then(|n| n.checked_add(u64::try_from(extra_records).ok()?.checked_mul(256)?))
            .and_then(|n| n.checked_add(512))
            .context("Task query copy bytes overflow")?;
        let mut material = (
            metadata_bytes(&self.coverage, self.records.len())?
                .checked_add(selector_bytes)
                .and_then(|n| n.checked_add(extra_bytes))
                .context("Task query copied material overflow")?,
            selector_count
                .checked_mul(4)
                .and_then(|n| n.checked_add(extra_records))
                .context("Task query copied nodes overflow")?,
        );
        for (_, record) in self
            .records
            .iter()
            .filter(|(key, _)| self.query_selects_key(query, key))
        {
            let (_, nodes) =
                super::super::input_evidence_budget::borrowed_json_material(&record.fact)?;
            material.0 = material
                .0
                .checked_add(u64::try_from(record.resident_bytes)?)
                .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
                .context("Task query selected body bytes overflow")?;
            material.1 = material
                .1
                .checked_add(nodes)
                .context("Task query selected body nodes overflow")?;
        }
        for (seq, record) in &self.stop_records {
            if !self.coverage.stop.heads.values().any(|head| {
                self.query_selects_task(query, &head.task.key)
                    && head
                        .head
                        .as_ref()
                        .is_some_and(|locator| locator.effect_seq == *seq)
            }) {
                continue;
            }
            let (_, nodes) =
                super::super::input_evidence_budget::borrowed_json_material(record.as_ref())?;
            material.0 = material
                .0
                .checked_add(record.resident_bytes()?)
                .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
                .context("Task query selected Stop bytes overflow")?;
            material.1 = material
                .1
                .checked_add(nodes)
                .context("Task query selected Stop nodes overflow")?;
        }
        let total = (
            prior
                .0
                .checked_add(material.0)
                .context("Task query prior bytes overflow")?,
            prior
                .1
                .checked_add(material.1)
                .context("Task query prior nodes overflow")?,
        );
        ensure!(
            total.0 <= u64::try_from(limits.0.min(self.byte_limit).min(64 * 1024 * 1024))?
                && total.1 <= limits.1.min(self.record_limit).min(65536),
            TaskReadError::ResourceExhausted("Task query joint capture")
        );
        // Only new closure/copy metadata is registered here. The original
        // capture still owns its selector, coverage and selected body guards.
        let extra = self
            .budget
            .as_ref()
            .map(|ledger| {
                ledger.register_owned_bytes(
                    &self.scope,
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: extra_bytes,
                    },
                )
            })
            .transpose()?;
        let mut captured = self.capture_query_inner(query)?;
        captured._budget_views.extend(extra);
        Ok((captured, material))
    }
    pub(crate) fn capture_query(&self, query: &TaskQuerySelection) -> Result<CapturedTasks> {
        self.capture_query_with_prior(query, (self.byte_limit, self.record_limit), (0, 0))
            .map(|(selected, _)| selected)
    }
    fn capture_query_inner(&self, query: &TaskQuerySelection) -> Result<CapturedTasks> {
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

#[cfg(test)]
mod borrowed_relation_tests {
    use super::*;
    use crate::{runtime::tests::support::LifecycleHarness, types::TaskKind};
    #[tokio::test]
    async fn borrowed_prediction_keeps_pruned_related_keys_and_rejects_a_changed_reverse_edge(
    ) -> Result<()> {
        let h = LifecycleHarness::new();
        let runtime = h.runtime();
        let at = chrono::Utc::now();
        let task = TaskRecord {
            id: "borrowed-original-task".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Queued,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        };
        runtime
            .persist_task_transition(&task, "task_queued")
            .await?;
        // Foreground Task publication also needs the refresh lock.
        let _pause = runtime.pause_resource_observer_for_test().await;
        let root = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("original root missing")?;
        let original = root
            .tasks
            .as_ref()
            .context("original Task domain missing")?;
        // A private read-only metadata boundary fixture, never a published
        // source or a new Ready grant. The body Arc comes from the actual W1.
        let mut domain = ResidentTaskDomain {
            scope: original.scope.clone(),
            coverage: original.coverage.clone(),
            records: original.records.clone(),
            stop_records: original.stop_records.clone(),
            record_limit: original.record_limit,
            byte_limit: original.byte_limit,
            budget: original.budget.clone(),
            budget_views: Vec::new(),
        };
        let key = TaskKey::new(TaskFamily::Task, &task.id);
        let pruned = TaskKey::new(TaskFamily::ResultSettlement, "original-pruned-result");
        domain
            .coverage
            .task_result_ids
            .get_mut(&task.id)
            .unwrap()
            .insert(pruned.id.clone());
        domain
            .coverage
            .observations
            .insert(pruned.clone(), TaskAvailability::Pruned);
        let query = TaskQuerySelection {
            required_keys: BTreeSet::from([key]),
            ..Default::default()
        };
        let original_closure = domain.closure(domain.coverage.query.selected_keys(&query)?)?;
        assert!(original_closure.contains(&pruned));
        for key in domain.coverage.observations.keys() {
            assert_eq!(
                domain.query_selects_key(&query, key),
                original_closure.contains(key)
            );
        }
        let (selected, material) =
            domain.capture_query_with_prior(&query, (64 * 1024 * 1024, 65536), (0, 0))?;
        assert_eq!(selected.records.len(), 1);
        assert!(Arc::ptr_eq(
            &selected.records[0],
            original
                .records
                .get(&TaskKey::new(TaskFamily::Task, &task.id))
                .unwrap()
        ));
        assert_eq!(
            selected.coverage.observations.get(&pruned),
            Some(&TaskAvailability::Pruned)
        );
        assert!(selected.lookup(&pruned)?.is_none());
        drop(selected);
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let error = domain
            .capture_query_with_prior(
                &query,
                (usize::try_from(material.0)? - 1, material.1),
                (0, 0),
            )
            .err()
            .context("Pruned closure scratch escaped the first gate")?;
        assert!(matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted(_))
        ));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        domain.coverage.result_metadata.insert(
            pruned.id.clone(),
            TaskResultMetadata {
                result_identity: pruned.id.clone(),
                task_id: "changed-target".into(),
                message_id: "original-result-message".into(),
                work_item_id: None,
                rejoin_generation: 1,
                parent_turn_id: "original-parent-turn".into(),
                state: TaskResultSettlementState::Settled,
                activation_id: None,
                activation_open: false,
                reply_message_id: None,
                created_at: at,
                next_recheck_at: None,
            },
        );
        let error = domain
            .capture_query_with_prior(&query, (64 * 1024 * 1024, 65536), (0, 0))
            .err()
            .context("changed reverse relation entered borrowed closure")?;
        assert!(error.to_string().contains("reverse relation changed"));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        Ok(())
    }
}
