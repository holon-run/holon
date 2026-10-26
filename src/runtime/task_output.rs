//! Exact Task output reads. Metadata polling is pure; body acquisition is named.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_input_evidence::{
        CapturedInputEvidence, InputEvidenceFamily, InputEvidenceKey, InputEvidenceReadError,
        PreparedInputEvidence,
    },
    resident_task::{
        CanonicalTaskFact, CapturedTasks, QualifiedTaskFact, TaskFamily, TaskKey, TaskReadError,
    },
    task_queries::{TaskQueryLease, TaskQuerySpec},
};
use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;

/// Only selected Arcs/Pins and owned accounting; no root/domain/Runtime backref.
#[derive(Debug)]
struct TaskOutputRead {
    tasks: Arc<CapturedTasks>,
    fact_index: usize,
    inputs: Option<CapturedInputEvidence>,
    _selectors: Arc<BudgetViewGuard>,
    material: OutputMaterialAllowance,
    _batch_material:
        Option<Arc<crate::runtime_db::input_evidence_snapshot::TaskOutputRecallMaterial>>,
}

/// An internal read carrier owns every selected/derived allocation until the
/// caller's final synchronous wire serialization. It carries no runtime/root.
pub(crate) struct TaskOutputResultRead {
    pub result: TaskOutputResult,
    _role: TaskQueryLease,
    _selected: TaskOutputRead,
    _render_budget: Arc<BudgetViewGuard>,
    _file_budget: Option<Arc<BudgetViewGuard>>,
}

struct TaskOutputSnapshotRead {
    snapshot: TaskOutputSnapshot,
    selected: TaskOutputRead,
    render_budget: Arc<BudgetViewGuard>,
    file_budget: Option<Arc<BudgetViewGuard>>,
    material: OutputMaterialAllowance,
}

#[derive(Debug, Clone, Copy)]
struct OutputMaterialAllowance {
    bytes: u64,
    records: usize,
    byte_limit: u64,
    record_limit: usize,
}

pub(crate) struct RunOnceTaskOutputRead {
    reads: Vec<TaskOutputRead>,
    material: OutputMaterialAllowance,
}

#[derive(Debug)]
pub(crate) struct RunOnceTaskOutputRetention {
    selected: Vec<TaskOutputRead>,
    render: Vec<Arc<BudgetViewGuard>>,
    files: Vec<Arc<BudgetViewGuard>>,
}

pub(crate) struct RunOnceTaskOutputs {
    pub(crate) snapshots: Vec<TaskOutputSnapshot>,
    pub(crate) retention: Arc<RunOnceTaskOutputRetention>,
}
impl OutputMaterialAllowance {
    fn new(limits: crate::config::StateMemoryLimits) -> Result<Self> {
        Ok(Self {
            bytes: 0,
            records: 0,
            byte_limit: limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("Task Output combined limit overflow")?,
            record_limit: usize::try_from(limits.record_limit)?,
        })
    }
    fn additional(self, bytes: u64, records: usize) -> Result<Self> {
        let next = Self {
            bytes: self
                .bytes
                .checked_add(bytes)
                .context("Task Output combined bytes overflow")?,
            records: self
                .records
                .checked_add(records)
                .context("Task Output combined count overflow")?,
            ..self
        };
        anyhow::ensure!(
            next.bytes <= next.byte_limit && next.records <= next.record_limit,
            TaskReadError::ResourceExhausted("Task Output combined retained/render/file material")
        );
        Ok(next)
    }
    fn remaining(self) -> (u64, usize) {
        (
            self.byte_limit - self.bytes,
            self.record_limit - self.records,
        )
    }
}
impl TaskOutputRead {
    fn fact(&self) -> &QualifiedTaskFact {
        &self.tasks.records[self.fact_index]
    }
    fn task(&self) -> &TaskRecord {
        let CanonicalTaskFact::Task(task) = &self.fact().fact else {
            unreachable!("selected Task family")
        };
        task
    }
    fn status(&self) -> Result<TaskStatus> {
        super::super::task_reads::task_output_status(&self.tasks, &self.task().id)
    }
    fn message(&self, id: &str) -> Result<&MessageEnvelope> {
        let key = InputEvidenceKey::new(InputEvidenceFamily::Message, id);
        let record = self
            .inputs
            .as_ref()
            .ok_or_else(|| InputEvidenceReadError::NeedsPreparation(key.clone()))?
            .lookup(&key)?
            .ok_or_else(|| InputEvidenceReadError::NeedsPreparation(key.clone()))?;
        anyhow::ensure!(
            record.locator.scope == self.tasks.scope,
            "Task output Input namespace differs"
        );
        let PreparedInputEvidence::Message(message) = &record.record else {
            anyhow::bail!("Task output Input family differs")
        };
        anyhow::ensure!(
            message.agent_id == self.tasks.scope.agent_id && message.id == id,
            "Task output Message scope differs"
        );
        Ok(message)
    }
    fn source(&self) -> Result<Option<&MessageEnvelope>> {
        let Some(head) = self.tasks.output_source(&self.task().id)? else {
            return Ok(None);
        };
        let key = &head.message.key;
        let message = self.message(&key.id)?;
        let actual = self
            .inputs
            .as_ref()
            .expect("validated body capture")
            .lookup(key)?
            .expect("validated body");
        anyhow::ensure!(
            actual.locator == head.message,
            "Task output accepted Source version differs from Input"
        );
        let metadata = message
            .metadata
            .as_ref()
            .context("Task output Source metadata missing")?;
        anyhow::ensure!(
            matches!(message.origin, MessageOrigin::Task { .. })
                && metadata.get("task_id").and_then(Value::as_str) == Some(self.task().id.as_str())
                && task_status_from_message(message, metadata) == head.message_status,
            "Task output Source differs from accepted Task/status metadata"
        );
        Ok(Some(message))
    }
}
impl RuntimeHandle {
    fn capture_task_output_query(
        &self,
        role: &TaskQueryLease,
        id: &str,
        bodies: bool,
    ) -> Result<TaskOutputRead> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Task output root is unpublished")?;
        let domain = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && domain.scope().agent_id == root.state.id
                && domain.scope().identity_incarnation == self.inner.identity_incarnation
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "Task output publication scope differs"
        );
        self.inner
            .task_queries
            .validate_lease(role, domain.scope(), root.token.owner_epoch)?;
        anyhow::ensure!(
            role.selection().output_task_ids.contains(id),
            "Task output role differs"
        );
        let tasks = domain.capture_query(role.selection())?;
        let key = TaskKey::new(TaskFamily::Task, id);
        tasks
            .lookup(&key)?
            .ok_or_else(|| task_not_found_error(id))?;
        let fact_index = tasks
            .records
            .iter()
            .position(|row| row.locator.key == key)
            .context("Task output body is missing")?;
        let CanonicalTaskFact::Task(task) = &tasks.records[fact_index].fact else {
            anyhow::bail!("Task output family differs")
        };
        let source = tasks.output_source(id)?;
        let reply =
            if task.kind == TaskKind::AgentMessageWait && task.status == TaskStatus::Completed {
                task.detail
                    .as_ref()
                    .and_then(|value| value.get("message_id"))
                    .and_then(Value::as_str)
            } else {
                None
            };
        let bytes =
            serialized_bytes(&(id, source.map(|head| head.message.key.id.as_str()), reply))?
                .checked_add(512)
                .and_then(|n| n.checked_mul(8))
                .context("Task output selector bytes overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        let (task_bytes, task_records) = tasks.selected_material_estimate()?;
        let (role_bytes, role_records) = role.material_estimate();
        let mut material = OutputMaterialAllowance::new(limits)?
            .additional(role_bytes, role_records)?
            .additional(task_bytes, task_records)?
            .additional(bytes, 24)?;
        let selectors = self.task_output_owned_budget(&tasks.scope, bytes, 24)?;
        let mut roots = BTreeSet::new();
        if bodies {
            roots.extend(source.map(|head| head.message.key.clone()));
            roots.extend(reply.map(|id| InputEvidenceKey::new(InputEvidenceFamily::Message, id)));
        }
        let (remaining_bytes, remaining_records) = material.remaining();
        anyhow::ensure!(
            roots.is_empty() || (remaining_bytes > 0 && remaining_records > 0),
            TaskReadError::ResourceExhausted("Task Output selected Input remaining material")
        );
        let request = InputEvidenceLoadRequest {
            query: None,
            windows: vec![],
            mandatory_roots: roots,
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit: remaining_records,
            byte_limit: usize::try_from(remaining_bytes)?,
        };
        let inputs =
            if request.mandatory_roots.is_empty() {
                None
            } else {
                let inputs = root.input_evidence.as_ref().ok_or_else(|| {
                    InputEvidenceReadError::NeedsPreparation(
                        request.mandatory_roots.first().unwrap().clone(),
                    )
                })?;
                anyhow::ensure!(
                    inputs.scope() == &tasks.scope,
                    "Task output Input publication scope differs"
                );
                Some(inputs.capture_task_output_for_request(
                    &tasks.records[fact_index].locator,
                    &request,
                )?)
            };
        if let Some(inputs) = &inputs {
            let (bytes, records) = inputs.selected_material_estimate()?;
            material = material.additional(bytes, records)?;
        }
        Ok(TaskOutputRead {
            tasks: Arc::new(tasks),
            fact_index,
            inputs,
            _selectors: selectors,
            material,
            _batch_material: None,
        })
    }
    /// Named final-product acquisition, never called by the polling path.
    /// The owner freezes all Task/Input/Event observations through one readonly
    /// transaction; the returned readers retain neither owner nor Root.
    pub(crate) async fn prepare_run_once_task_outputs(
        &self,
        session: &crate::runtime::task_queries::TaskBirthSession,
    ) -> Result<(
        crate::runtime::task_queries::RunOnceRead,
        RunOnceTaskOutputRead,
    )> {
        let owner = self.begin_mutation().await?;
        let read = self.capture_run_once(session)?;
        let (mut request, _request_material) = owner.owner_task_load_request()?;
        let count = read.task_ids()?.count();
        let selector_bytes = serialized_bytes(&read.tasks.scope)?
            .checked_add(
                u64::try_from(count)?
                    .checked_mul(u64::try_from(
                        std::mem::size_of::<TaskOutputRead>()
                            + std::mem::size_of::<TaskOutputSnapshot>()
                            + 512,
                    )?)
                    .context("Output batch selector size overflow")?,
            )
            .and_then(|n| n.checked_mul(4))
            .context("Output batch selector size overflow")?;
        let selector_records = count
            .checked_mul(8)
            .context("Output batch selectors overflow")?;
        let selectors =
            self.task_output_owned_budget(&read.tasks.scope, selector_bytes, selector_records)?;
        let _ids = self.task_session_id_budget(session, read.task_ids()?)?;
        let ids = read.task_ids()?.collect::<Vec<_>>();
        let (task_bytes, task_records) = read.tasks.selected_material_estimate()?;
        let mut material = OutputMaterialAllowance {
            bytes: 0,
            records: 0,
            byte_limit: u64::try_from(request.byte_limit)?,
            record_limit: request.record_limit,
        }
        .additional(task_bytes, task_records)?
        .additional(selector_bytes, selector_records)?;
        let (bytes, records) = material.remaining();
        request.byte_limit = usize::try_from(bytes)?;
        request.record_limit = records;
        let mut batch = self.runtime_db().transaction(|tx| {
            crate::runtime_db::task_snapshot::prepare_task_output_input_batch_tx(
                tx,
                &read.state,
                &read.tasks,
                &ids,
                &request,
                &self.inner.input_evidence_budget,
            )
        })?;
        let mut reads = Vec::with_capacity(count);
        if let Some(batch) = &mut batch {
            for id in ids {
                let fact_index = read
                    .tasks
                    .records
                    .iter()
                    .position(|task| {
                        task.locator.key.family == TaskFamily::Task && task.locator.key.id == id
                    })
                    .context("Output batch Task body missing")?;
                let inputs = batch.capture(
                    &read.tasks.records[fact_index].locator,
                    &self.inner.input_evidence_budget,
                )?;
                reads.push(TaskOutputRead {
                    tasks: read.tasks.clone(),
                    fact_index,
                    inputs: Some(inputs),
                    _selectors: selectors.clone(),
                    material,
                    _batch_material: None,
                });
            }
        }
        if let Some(batch) = batch {
            let (bytes, records) = batch.admitted_material();
            material = material.additional(u64::try_from(bytes)?, records)?;
            let retained = batch.into_material();
            for read in &mut reads {
                read._batch_material = Some(retained.clone());
            }
        }
        // Same stable time ordering as the original ID-ordered Task listing.
        reads.sort_by(|left, right| left.task().created_at.cmp(&right.task().created_at));
        Ok((read, RunOnceTaskOutputRead { reads, material }))
    }

    pub(crate) async fn render_run_once_task_outputs(
        &self,
        prepared: RunOnceTaskOutputRead,
    ) -> Result<RunOnceTaskOutputs> {
        let count = prepared.reads.len();
        let mut material = prepared.material;
        let mut snapshots = Vec::with_capacity(count);
        let mut retention = RunOnceTaskOutputRetention {
            selected: Vec::with_capacity(count),
            render: Vec::with_capacity(count),
            files: Vec::with_capacity(count),
        };
        for mut read in prepared.reads {
            read.material = material;
            let rendered = self.task_output_snapshot_from_read(read).await?;
            material = rendered.material;
            snapshots.push(rendered.snapshot);
            retention.selected.push(rendered.selected);
            retention.render.push(rendered.render_budget);
            retention.files.extend(rendered.file_budget);
        }
        Ok(RunOnceTaskOutputs {
            snapshots,
            retention: Arc::new(retention),
        })
    }

    fn task_output_owned_budget(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        bytes: u64,
        records: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            u64::try_from(records)? <= limits.record_limit
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("Task output material limits overflow")?,
            TaskReadError::ResourceExhausted("Task output owned material")
        );
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?)
    }
    pub async fn task_output(
        &self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> Result<TaskOutputResult> {
        let read = self.task_output_read(task_id, block, timeout_ms).await?;
        Ok(read.result)
    }

    pub(crate) async fn task_output_read(
        &self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> Result<TaskOutputResultRead> {
        let role = self
            .with_task_publication(|| {
                self.register_task_query(TaskQuerySpec {
                    output_task_ids: &[task_id],
                    ..Default::default()
                })
            })
            .await?;
        let started_at = tokio::time::Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let mut prepared_historical_task = false;
        loop {
            let read = match self
                .with_task_publication(|| self.capture_task_output_query(&role, task_id, false))
                .await
            {
                Ok(read) => read,
                Err(error)
                    if !prepared_historical_task
                        && matches!(error.downcast_ref::<TaskReadError>(),
                    Some(TaskReadError::NeedsPreparation(key)) if key.family==TaskFamily::Task && key.id==task_id) =>
                {
                    // Explicit output of a target outside the maintained Task window.
                    let mut owner = self.begin_mutation().await?;
                    owner.prepare_owner_task_query(&role).await?;
                    prepared_historical_task = true;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let ready = task_output_ready(read.task(), &read.status()?);
            if !ready && block && started_at.elapsed() < timeout {
                drop(read);
                tokio::time::sleep(
                    Duration::from_millis(TASK_OUTPUT_POLL_INTERVAL_MS)
                        .min(timeout.saturating_sub(started_at.elapsed())),
                )
                .await;
                continue;
            }
            drop(read);
            // Full/historical output is an explicit body acquisition cause;
            // metadata polls and repeated prepared reads remain pure captures.
            let read = match self
                .with_task_publication(|| self.capture_task_output_query(&role, task_id, true))
                .await
            {
                Ok(read) => read,
                Err(error)
                    if matches!(
                        error.downcast_ref::<InputEvidenceReadError>(),
                        Some(InputEvidenceReadError::NeedsPreparation(_))
                    ) =>
                {
                    let mut owner = self.begin_mutation().await?;
                    owner.prepare_task_output_inputs(&role, task_id).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let ready = task_output_ready(read.task(), &read.status()?);
            if !ready && block && started_at.elapsed() < timeout {
                drop(read);
                continue;
            }
            let retrieval_status = if ready {
                TaskOutputRetrievalStatus::Success
            } else if block {
                TaskOutputRetrievalStatus::Timeout
            } else {
                TaskOutputRetrievalStatus::NotReady
            };
            let rendered = self.task_output_snapshot_from_read(read).await?;
            return Ok(TaskOutputResultRead {
                result: TaskOutputResult {
                    retrieval_status,
                    task: rendered.snapshot,
                },
                _role: role,
                _selected: rendered.selected,
                _render_budget: rendered.render_budget,
                _file_budget: rendered.file_budget,
            });
        }
    }
    async fn task_output_snapshot_from_read(
        &self,
        read: TaskOutputRead,
    ) -> Result<TaskOutputSnapshotRead> {
        anyhow::ensure!(
            read.tasks.scope.agent_id == self.inner.mutation_owner.agent_id()
                && read.tasks.scope.identity_incarnation == self.inner.identity_incarnation,
            "Task output rendering runtime differs"
        );
        read._selectors.validate_owned_material_binding(
            &read.tasks.scope,
            &self.inner.input_evidence_budget,
            0,
        )?;
        if let Some(material) = &read._batch_material {
            material.validate_ledger(&self.inner.input_evidence_budget)?;
        }
        let projection = read.tasks.task_status_projection(&read.task().id)?;
        let mut bytes = serialized_bytes(&(read.task(), projection))?
            .checked_add(1024)
            .context("Task output render bytes overflow")?;
        if let Some(inputs) = &read.inputs {
            for row in &inputs.records {
                if let PreparedInputEvidence::Message(message) = &row.record {
                    bytes = bytes
                        .checked_add(serialized_bytes(message)?)
                        .context("Task output Message bytes overflow")?;
                }
            }
        }
        let render_bytes = bytes
            .checked_mul(8)
            .context("Task output copies overflow")?;
        let render_records = read
            .inputs
            .as_ref()
            .map_or(1, |inputs| inputs.records.len() + 1)
            .checked_mul(8)
            .context("Task output count overflow")?;
        let mut material = read.material.additional(render_bytes, render_records)?;
        let render_budget =
            self.task_output_owned_budget(&read.tasks.scope, render_bytes, render_records)?;
        let task = projection.overlay(read.fact())?;
        let latest_message = read.source()?.map(|message| TaskMessageSnapshot {
            state: task_status_from_message(
                message,
                message
                    .metadata
                    .as_ref()
                    .expect("validated Source metadata"),
            ),
            text: render_task_message_body(&message.body),
        });
        let status = effective_task_output_status(&task.status, latest_message.as_ref());
        let summary = task.summary.clone();

        let mut file_budget = None;
        let (full_output, output_path, result_summary, exit_status) =
            if task.kind == TaskKind::CommandTask {
                let output_path = detail_string(&task.detail, "output_path");
                let (output, budget, spent) = self
                    .read_command_task_output(
                        &read.tasks.scope,
                        &task,
                        output_path.as_deref(),
                        material,
                    )
                    .await?;
                file_budget = budget;
                material = spent;
                let result_summary = detail_string(&task.detail, "output_summary")
                    .or_else(|| latest_message.as_ref().map(|message| message.text.clone()));
                let exit_status = task
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("exit_status"))
                    .and_then(|value| value.as_i64())
                    .and_then(|value| i32::try_from(value).ok());
                (output, output_path, result_summary, exit_status)
            } else if task.kind == TaskKind::AgentMessageWait
                && task.status == TaskStatus::Completed
                && detail_string(&task.detail, "message_id").is_some()
            {
                let reply_id =
                    detail_string(&task.detail, "message_id").expect("checked reply reference");
                let reply = read.message(&reply_id)?;
                // Retrieval dereferences the original evidence; it does not
                // publish a second copy into the runtime message stream.
                (
                    render_task_message_body(&reply.body),
                    None,
                    detail_string(&task.detail, "output_summary"),
                    None,
                )
            } else {
                let output = latest_message
                    .as_ref()
                    .map(|message| message.text.clone())
                    .or_else(|| detail_string(&task.detail, "output_summary"))
                    .unwrap_or_else(|| summary.clone().unwrap_or_default());
                let result_summary = if output.trim().is_empty() {
                    None
                } else {
                    Some(output.clone())
                };
                (output, None, result_summary, None)
            };
        let (output_preview, output_truncated) =
            truncate_output_to_char_budget(&full_output, TASK_OUTPUT_PREVIEW_CHAR_BUDGET);
        // The explicit output acquisition, not a stored path hint, proves that
        // a command output artifact is available.
        let artifact_path = output_path
            .as_deref()
            .filter(|_| task.kind != TaskKind::CommandTask || file_budget.is_some());
        let (artifacts, output_artifact) = task_output_artifacts(artifact_path);
        let failure_artifact = task_failure_artifact(
            &task,
            &status,
            full_output.as_str(),
            artifact_path,
            exit_status,
        );

        let child_supervision = match projection.child_supervision(read.fact())? {
            Some(projection) => Some(self.task_child_supervision(projection).await?),
            None => None,
        };

        let token_usage = task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("token_usage"))
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        let output_capture = task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("output_capture"))
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok());

        let snapshot = TaskOutputSnapshot {
            task_id: task.id,
            kind: task.kind.as_str().to_string(),
            status,
            summary,
            output_preview,
            output_truncated,
            artifacts,
            output_artifact,
            result_summary,
            exit_status,
            output_capture,
            failure_artifact,
            child_supervision,
            token_usage,
        };
        Ok(TaskOutputSnapshotRead {
            snapshot,
            selected: read,
            render_budget,
            file_budget,
            material,
        })
    }

    async fn read_command_task_output(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        task: &TaskRecord,
        output_path: Option<&str>,
        material: OutputMaterialAllowance,
    ) -> Result<(
        String,
        Option<Arc<BudgetViewGuard>>,
        OutputMaterialAllowance,
    )> {
        use tokio::io::AsyncReadExt;
        let tokens = task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("max_output_tokens"))
            .and_then(Value::as_u64)
            .map(|value| value as usize);
        let fallback = detail_string(&task.detail, "initial_output")
            .or_else(|| detail_string(&task.detail, "output_summary"))
            .unwrap_or_default();
        let fallback_output =
            || crate::tool::helpers::truncate_output_for_tokens(&fallback, tokens);
        let Some(path) = output_path else {
            return Ok((fallback_output(), None, material));
        };
        let file = match tokio::fs::File::open(path).await {
            Ok(file) => file,
            Err(_) => return Ok((fallback_output(), None, material)),
        };
        let len = match file.metadata().await {
            Ok(meta) => meta.len(),
            Err(_) => return Ok((fallback_output(), None, material)),
        };
        let limit = len
            .checked_add(1)
            .context("Task output file length overflow")?;
        let bytes = limit
            .checked_add(256)
            .and_then(|n| n.checked_mul(8))
            .context("Task output file copies overflow")?;
        let remaining = material.additional(bytes, 8)?;
        let guard = self.task_output_owned_budget(scope, bytes, 8)?;
        let mut content = Vec::with_capacity(usize::try_from(limit)?);
        match file.take(limit).read_to_end(&mut content).await {
            Ok(_) if u64::try_from(content.len())? > len => Err(TaskReadError::ResourceExhausted(
                "Task output file grew during acquisition",
            )
            .into()),
            Ok(_) if !content.is_empty() || fallback.is_empty() => Ok((
                crate::tool::helpers::truncate_output_for_tokens(
                    &String::from_utf8_lossy(&content),
                    tokens,
                ),
                Some(guard),
                remaining,
            )),
            _ => Ok((fallback_output(), Some(guard), remaining)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::LifecycleHarness;
    fn task(id: &str) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        }
    }
    #[tokio::test]
    async fn exact_output_source_and_status_repeat_from_one_root_with_all_body_tables_forbidden(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let task = task("pure-output-source");
        let role = runtime.register_task_query(TaskQuerySpec {
            output_task_ids: &[task.id.as_str()],
            ..Default::default()
        })?;
        let mut source = MessageEnvelope::new(
            "default",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "original exact source body".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        source.task_id = Some(task.id.clone());
        source.metadata = Some(
            serde_json::json!({"task_id":task.id,"task_kind":"command_task","task_status":"completed"}),
        );
        runtime
            .persist_task_transition_with_message(&task, "task_queued", &source)
            .await?;
        {
            let mut owner = runtime.begin_mutation().await?;
            owner.prepare_task_output_inputs(&role, &task.id).await?;
        }
        let read = runtime.capture_task_output_query(&role, &task.id, true)?;
        assert_eq!(read.status()?, TaskStatus::Completed);
        assert_eq!(
            render_task_message_body(&read.source()?.unwrap().body),
            "original exact source body"
        );
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE tasks RENAME TO output_task_body_forbidden;
            ALTER TABLE messages RENAME TO output_message_body_forbidden;
            ALTER TABLE agent_states RENAME TO output_state_body_forbidden;
            ALTER TABLE agent_identities RENAME TO output_identity_body_forbidden;",
            )?;
            Ok(())
        })?;
        let repeated = runtime.capture_task_output_query(&role, &task.id, true)?;
        assert_eq!(repeated.source()?.unwrap(), read.source()?.unwrap());
        assert_eq!(repeated.status()?, TaskStatus::Completed);
        drop(repeated);
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        let mut drift = runtime.capture_task_output_query(&role, &task.id, true)?;
        Arc::get_mut(&mut drift.tasks)
            .expect("single owning test capture")
            .coverage
            .output
            .heads
            .get_mut(&task.id)
            .unwrap()
            .source
            .as_mut()
            .unwrap()
            .message
            .content_digest = "a".repeat(64);
        assert!(drift
            .source()
            .unwrap_err()
            .to_string()
            .contains("accepted Source version differs"));
        let metadata = runtime.capture_task_output_query(&role, &task.id, false)?;
        assert!(matches!(
            metadata
                .source()
                .unwrap_err()
                .downcast_ref::<InputEvidenceReadError>(),
            Some(InputEvidenceReadError::NeedsPreparation(_))
        ));
        Ok(())
    }
    #[tokio::test]
    async fn completed_message_wait_dereferences_original_non_task_reply_from_selected_inputs(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let reply = runtime
            .enqueue(
                MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: Some("control".into()),
                        actor_display_name: None,
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Next,
                    MessageBody::Text {
                        text: "original reply evidence only once".into(),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::HttpControlPrompt,
                    AdmissionContext::ControlAuthenticated,
                ),
            )
            .await?;
        let mut task = task("completed-original-reply");
        task.kind = TaskKind::AgentMessageWait;
        task.status = TaskStatus::Completed;
        task.detail = Some(serde_json::json!({"message_id":reply.id}));
        let role = runtime.register_task_query(TaskQuerySpec {
            output_task_ids: &[task.id.as_str()],
            ..Default::default()
        })?;
        runtime
            .persist_task_transition(&task, "task_completed")
            .await?;
        let original_count = runtime.storage().read_all_messages()?.len();
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE tasks RENAME TO reply_output_task_forbidden;
            ALTER TABLE messages RENAME TO reply_output_message_forbidden;",
            )?;
            Ok(())
        })?;
        let read = runtime.capture_task_output_query(&role, &task.id, true)?;
        assert!(read.source()?.is_none());
        assert_eq!(
            render_task_message_body(&read.message(&reply.id)?.body),
            "original reply evidence only once"
        );
        assert_eq!(
            runtime.runtime_db().transaction(|tx| Ok(tx.query_row(
                "SELECT COUNT(*) FROM reply_output_message_forbidden",
                [],
                |row| row.get::<_, usize>(0)
            )?))?,
            original_count
        );
        Ok(())
    }
    #[tokio::test]
    async fn explicit_file_output_checks_actual_length_before_buffer_allocation() -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let task = task("file-budget-output");
        runtime
            .persist_task_transition(&task, "task_queued")
            .await?;
        let role = runtime.register_task_query(TaskQuerySpec {
            output_task_ids: &[task.id.as_str()],
            ..Default::default()
        })?;
        let read = runtime.capture_task_output_query(&role, &task.id, false)?;
        let home = tempfile::tempdir()?;
        let path = home.path().join("large-output");
        let file = std::fs::File::create(&path)?;
        let limits = runtime.inner.input_evidence_budget.limits();
        file.set_len(limits.history_per_agent_bytes + limits.pending_per_agent_bytes + 1)?;
        drop(file);
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let error = runtime
            .read_command_task_output(
                &read.tasks.scope,
                read.task(),
                Some(path.to_str().unwrap()),
                read.material,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted(_))
        ));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        Ok(())
    }
    #[tokio::test]
    async fn held_render_material_and_full_file_use_one_remaining_output_budget() -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let task = task("combined-file-output");
        runtime
            .persist_task_transition(&task, "task_queued")
            .await?;
        let role = runtime.register_task_query(TaskQuerySpec {
            output_task_ids: &[&task.id],
            ..Default::default()
        })?;
        let read = runtime.capture_task_output_query(&role, &task.id, false)?;
        let size = 6 * 1024 * 1024usize;
        // The same owned rendered-message DTO used by the real renderer. Its
        // conservative guard precedes allocation and stays across file await.
        let render_guard = runtime.task_output_owned_budget(
            &read.tasks.scope,
            u64::try_from(size + 512)?.checked_mul(8).unwrap(),
            8,
        )?;
        let rendered = TaskMessageSnapshot {
            state: TaskStatus::Completed,
            text: "x".repeat(size),
        };
        let material = read.material.additional(
            serialized_bytes(&(&rendered.state, &rendered.text))?
                .checked_mul(8)
                .unwrap(),
            8,
        )?;
        let home = tempfile::tempdir()?;
        let path = home.path().join("combined-output");
        let file = std::fs::File::create(&path)?;
        file.set_len(u64::try_from(size)?)?;
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let error = runtime
            .read_command_task_output(
                &read.tasks.scope,
                read.task(),
                Some(path.to_str().unwrap()),
                material,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<TaskReadError>(),
                Some(TaskReadError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline,
            "combined rejection precedes file buffer/guard acquisition"
        );
        file.set_len(1024)?;
        drop(file);
        let (output, file_guard, _) = runtime
            .read_command_task_output(
                &read.tasks.scope,
                read.task(),
                Some(path.to_str().unwrap()),
                material,
            )
            .await?;
        assert_eq!(output.len(), 1024);
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
        drop((output, file_guard));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        drop((rendered, render_guard, read, role));
        Ok(())
    }

    #[test]
    fn output_remaining_material_rejects_count_and_overflow_without_mutating_the_prior_view(
    ) -> Result<()> {
        let original = OutputMaterialAllowance {
            bytes: 32,
            records: 3,
            byte_limit: 64,
            record_limit: 4,
        };
        assert!(original.additional(1, 2).is_err());
        assert!(original.additional(u64::MAX, 0).is_err());
        assert_eq!(original.remaining(), (32, 1));
        assert_eq!(original.additional(32, 1)?.remaining(), (0, 0));
        Ok(())
    }
    #[tokio::test]
    async fn actual_output_result_carrier_keeps_material_until_tool_and_http_serialization(
    ) -> Result<()> {
        use axum::response::IntoResponse;
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let home = tempfile::tempdir()?;
        let path = home.path().join("actual-wire-output.txt");
        let file_text = "accepted file output\n".repeat(256);
        std::fs::write(&path, &file_text)?;
        let mut task = task("actual-output-wire-carrier");
        task.detail = Some(serde_json::json!({"output_path":path.to_str().unwrap()}));
        let mut source = MessageEnvelope::new(
            "default",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "accepted source output".repeat(128),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        source.task_id = Some(task.id.clone());
        source.metadata = Some(serde_json::json!({
            "task_id":task.id,"task_kind":"command_task","task_status":"completed",
        }));
        runtime
            .persist_task_transition_with_message(&task, "task_queued", &source)
            .await?;
        let warmed = runtime
            .managed_tasks()
            .task_output_read(&task.id, false, 0)
            .await?;
        drop(warmed);
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let read = runtime
            .managed_tasks()
            .task_output_read(&task.id, false, 0)
            .await?;
        assert_eq!(read.result.task.status, TaskStatus::Completed);
        assert_eq!(read.result.task.output_preview, file_text);
        let render = Arc::downgrade(&read._render_budget);
        let selected = Arc::downgrade(&read._selected.tasks);
        let selectors = Arc::downgrade(&read._selected._selectors);
        let file = Arc::downgrade(
            read._file_budget
                .as_ref()
                .context("actual file guard missing")?,
        );
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
        let tool_wire =
            crate::tool::tools::serialize_success(crate::tool::names::TASK_OUTPUT, &read.result)?;
        let http_wire = axum::Json(&read.result).into_response();
        assert_eq!(http_wire.status(), axum::http::StatusCode::OK);
        assert_eq!(
            tool_wire.envelope.result.as_ref().unwrap()["task"]["output_preview"],
            file_text
        );
        assert!(render.upgrade().is_some() && file.upgrade().is_some());
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline,
            "actual result materials stay observed after await and through synchronous serialization");
        drop(read);
        assert!(render.upgrade().is_none() && file.upgrade().is_none());
        assert!(selected.upgrade().is_none() && selectors.upgrade().is_none());
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline,
            "only final wire values escape after the internal carrier is released"
        );
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE tasks RENAME TO wire_task_body_forbidden;
                ALTER TABLE messages RENAME TO wire_message_body_forbidden;
                ALTER TABLE agent_states RENAME TO wire_state_body_forbidden;
                ALTER TABLE agent_identities RENAME TO wire_identity_body_forbidden;",
            )?;
            Ok(())
        })?;
        let actual_tool = crate::tool::tools::task_output::execute(
            &runtime,
            "default",
            &AuthorityClass::RuntimeInstruction,
            &serde_json::json!({"task_id":task.id,"block":false}),
        )
        .await?;
        assert_eq!(
            actual_tool.envelope.result.as_ref().unwrap()["task"]["output_preview"],
            file_text
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        drop((tool_wire, http_wire, actual_tool));
        Ok(())
    }
}

#[cfg(test)]
#[path = "run_once_task_outputs_tests.rs"]
mod run_once_task_outputs_tests;
