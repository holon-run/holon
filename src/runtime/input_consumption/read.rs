//! Named, finite preparation. The borrowed transaction is the selection cut;
//! SQL addresses never qualify Message, Turn, Work, or Task bodies.
use super::*;
use crate::{
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::{
        resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence as Fact,
        },
        resident_task::{
            CanonicalTaskFact, QualifiedTaskFact, TaskCaptureRequest, TaskFamily, TaskKey,
        },
        resident_work::{WorkCaptureRequest, WorkFamily, WorkKey},
    },
    runtime_db::{
        context_inputs::{ContextDurableRead, ContextScopeRead},
        input_evidence_snapshot::{prepare_named_input_evidence_tx, InputEvidenceLoadRequest},
        task_result_settlement::{self as results, TaskResultSettlementRecord},
        task_snapshot::{preflight_named_task_snapshot_tx, TaskDomainLoadRequest},
        work_snapshot::{preflight_named_work_snapshot_tx, WorkDomainLoadRequest},
    },
};
use rusqlite::{params, OptionalExtension, Transaction};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

#[derive(Debug, Clone)]
pub(in crate::runtime) struct QualifiedQueueRead(
    Arc<crate::runtime_db::current_read_writes::OwnedCurrentRecord>,
);

impl std::ops::Deref for QualifiedQueueRead {
    type Target = QueueEntryRecord;

    fn deref(&self) -> &Self::Target {
        match self.0.record() {
            crate::runtime_db::current_read_writes::CurrentRecord::Queue(record) => record,
            _ => unreachable!("idle Queue has another family"),
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::runtime) struct QualifiedResultRead {
    fact: Arc<QualifiedTaskFact>,
    _capture: Arc<crate::runtime::resident_task::CapturedTasks>,
}

impl std::ops::Deref for QualifiedResultRead {
    type Target = TaskResultSettlementRecord;

    fn deref(&self) -> &Self::Target {
        match &self.fact.fact {
            CanonicalTaskFact::ResultSettlement(record) => record,
            _ => unreachable!("idle Result has another family"),
        }
    }
}

impl std::borrow::Borrow<TaskResultSettlementRecord> for QualifiedResultRead {
    fn borrow(&self) -> &TaskResultSettlementRecord {
        self
    }
}

#[derive(Debug, Clone)]
pub(in crate::runtime) struct QualifiedMessageRead {
    fact: Arc<crate::runtime::resident_input_evidence::QualifiedInputEvidence>,
    _capture: Arc<crate::runtime::resident_input_evidence::CapturedInputEvidence>,
}

impl std::ops::Deref for QualifiedMessageRead {
    type Target = MessageEnvelope;

    fn deref(&self) -> &Self::Target {
        match &self.fact.record {
            Fact::Message(message) => message,
            _ => unreachable!("idle Message has another family"),
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::runtime) struct QualifiedTurnRead {
    fact: Arc<crate::runtime::resident_input_evidence::QualifiedInputEvidence>,
    _capture: Arc<crate::runtime::resident_input_evidence::CapturedInputEvidence>,
}

impl QualifiedTurnRead {
    pub(in crate::runtime) fn from_capture(
        capture: Arc<crate::runtime::resident_input_evidence::CapturedInputEvidence>,
        id: &str,
    ) -> Result<Self> {
        let fact = capture
            .lookup_borrowed(Family::Turn, id)?
            .context("idle capture omitted committed Turn")?;
        anyhow::ensure!(
            fact.locator.scope == capture.scope
                && matches!(&fact.record, Fact::Turn(turn)
                    if turn.agent_id == capture.scope.agent_id && turn.turn_id == id),
            "idle Turn scope/family mismatch"
        );
        Ok(Self {
            fact: fact.clone(),
            _capture: capture,
        })
    }
}

impl std::ops::Deref for QualifiedTurnRead {
    type Target = TurnRecord;

    fn deref(&self) -> &Self::Target {
        match &self.fact.record {
            Fact::Turn(turn) => turn,
            _ => unreachable!("idle Turn has another family"),
        }
    }
}

pub(in crate::runtime) struct IdleRead<'a, 'connection> {
    pub runtime: &'a RuntimeHandle,
    pub tx: &'a Transaction<'connection>,
    pub scope: crate::runtime::resident_input_evidence::InputEvidenceScope,
    scope_read: ContextScopeRead<'a>,
    input_reads: RefCell<
        BTreeMap<
            BTreeSet<Key>,
            Arc<crate::runtime::resident_input_evidence::CapturedInputEvidence>,
        >,
    >,
    task_reads: RefCell<BTreeMap<TaskKey, Arc<crate::runtime::resident_task::CapturedTasks>>>,
    work_reads: RefCell<BTreeMap<String, Arc<crate::runtime::resident_work::CapturedWork>>>,
    #[cfg(test)]
    pending_previews: RefCell<
        BTreeMap<
            String,
            (
                QualifiedResultRead,
                crate::context::PendingTaskResultPreview,
            ),
        >,
    >,
    record_limit: usize,
    byte_limit: usize,
}

impl<'a, 'connection> IdleRead<'a, 'connection> {
    fn new(
        runtime: &'a RuntimeHandle,
        tx: &'a Transaction<'connection>,
        expected: Option<&AgentState>,
    ) -> Result<Self> {
        let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: runtime.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: runtime.inner.identity_incarnation,
        };
        let facts =
            ContextDurableRead::new(tx, runtime.inner.mutation_owner.agent_id()).scope_read()?;
        let identity = facts
            .facts()
            .identity
            .as_ref()
            .context("idle read identity missing")?;
        anyhow::ensure!(
            identity.agent_id == scope.agent_id
                && identity.incarnation == scope.identity_incarnation
                && identity.status == crate::types::AgentRegistryStatus::Active,
            "idle read scope retired"
        );
        let state = facts
            .facts()
            .state
            .as_deref()
            .context("idle read State missing")?;
        anyhow::ensure!(
            expected.is_none_or(|expected| expected == state),
            "idle read differs from held owner State"
        );
        let limits = runtime.inner.input_evidence_budget.limits();
        Ok(Self {
            runtime,
            tx,
            scope,
            scope_read: facts,
            input_reads: RefCell::new(BTreeMap::new()),
            task_reads: RefCell::new(BTreeMap::new()),
            work_reads: RefCell::new(BTreeMap::new()),
            #[cfg(test)]
            pending_previews: RefCell::new(BTreeMap::new()),
            record_limit: usize::try_from(limits.record_limit.min(65_536))?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("idle read byte limit overflow")?
                    .min(64 * 1024 * 1024),
            )?,
        })
    }

    fn input_request(&self) -> InputEvidenceLoadRequest {
        InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit: self.record_limit,
            byte_limit: self.byte_limit,
        }
    }

    fn inputs(
        &self,
        request: &InputEvidenceLoadRequest,
    ) -> Result<Arc<crate::runtime::resident_input_evidence::CapturedInputEvidence>> {
        if let Some(captured) = self.input_reads.borrow().get(&request.mandatory_roots) {
            self.scope_read.validate(self.tx, &self.scope)?;
            return Ok(captured.clone());
        }
        let resident =
            prepare_named_input_evidence_tx(self.tx, &self.scope, request, &self.scope_read)?;
        let captured = resident
            .with_budget(self.runtime.inner.input_evidence_budget.clone())?
            .capture_for_request(request)?;
        resource_io::admit_cloned_value(&request.mandatory_roots)?;
        resource_io::charge_owned_metadata(
            u64::try_from(std::mem::size_of_val(&captured))?.saturating_add(128),
            3,
        )?;
        let captured = Arc::new(captured);
        self.input_reads
            .borrow_mut()
            .insert(request.mandatory_roots.clone(), captured.clone());
        Ok(captured)
    }

    fn fact(
        &self,
        family: Family,
        id: &str,
    ) -> Result<Option<Arc<crate::runtime::resident_input_evidence::QualifiedInputEvidence>>> {
        self.scope_read.validate(self.tx, &self.scope)?;
        // Earlier named reads may already own this qualified closure member.
        // Reuse only coverage proved on this same non-writing transaction.
        for captured in self.input_reads.borrow().values() {
            if let Some(record) = captured
                .records
                .iter()
                .find(|record| record.locator.key.family == family && record.locator.key.id == id)
            {
                return Ok(Some(record.clone()));
            }
            if captured
                .unavailable
                .iter()
                .any(|key| key.family == family && key.id == id)
            {
                return Ok(None);
            }
        }
        let mut request = self.input_request();
        request.insert_admitted_root(family, id)?;
        Ok(self
            .inputs(&request)?
            .lookup(&Key::new(family, id))?
            .cloned())
    }

    /// The closure cannot carry a body reference beyond its qualified capture.
    pub(in crate::runtime) fn with_message<T>(
        &self,
        id: &str,
        read: impl FnOnce(&MessageEnvelope) -> Result<T>,
    ) -> Result<Option<T>> {
        resource_io::check_active()?;
        self.fact(Family::Message, id)?
            .map(|row| {
                let Fact::Message(value) = &row.record else {
                    bail!("idle Message has another family");
                };
                read(value)
            })
            .transpose()
    }

    /// Owning consumers still admit their full copy before allocating it.
    #[cfg(test)]
    pub(in crate::runtime) fn message(&self, id: &str) -> Result<Option<MessageEnvelope>> {
        self.with_message(id, |value| {
            resource_io::admit_cloned_value(value)?;
            Ok(value.clone())
        })
    }
    pub(in crate::runtime) fn shared_message(
        &self,
        id: &str,
    ) -> Result<Option<QualifiedMessageRead>> {
        resource_io::check_active()?;
        let Some(fact) = self.fact(Family::Message, id)? else {
            return Ok(None);
        };
        anyhow::ensure!(
            fact.locator.scope == self.scope
                && matches!(&fact.record, Fact::Message(message)
                    if message.agent_id == self.scope.agent_id && message.id == id),
            "idle Message scope/family mismatch"
        );
        let capture = self
            .input_reads
            .borrow()
            .values()
            .find(|capture| capture.records.iter().any(|row| Arc::ptr_eq(row, &fact)))
            .cloned()
            .context("idle Message omitted its qualified capture")?;
        Ok(Some(QualifiedMessageRead {
            fact,
            _capture: capture,
        }))
    }
    pub(in crate::runtime) fn turn(&self, id: &str) -> Result<Option<TurnRecord>> {
        self.shared_turn(id)?
            .map(|turn| {
                resource_io::admit_cloned_value(&*turn)?;
                Ok((*turn).clone())
            })
            .transpose()
    }
    pub(in crate::runtime) fn shared_turn(&self, id: &str) -> Result<Option<QualifiedTurnRead>> {
        resource_io::check_active()?;
        self.scope_read.validate(self.tx, &self.scope)?;
        if !crate::runtime_db::input_evidence_snapshot::optional_turn_present_tx(
            self.tx,
            &self.scope,
            id,
        )? {
            return Ok(None);
        }
        let Some(fact) = self.fact(Family::Turn, id)? else {
            return Ok(None);
        };
        let capture = self
            .input_reads
            .borrow()
            .values()
            .find(|capture| capture.records.iter().any(|row| Arc::ptr_eq(row, &fact)))
            .cloned()
            .context("idle Turn omitted its qualified capture")?;
        QualifiedTurnRead::from_capture(capture, id).map(Some)
    }
    pub(in crate::runtime) fn protocol(
        &self,
    ) -> Result<Option<crate::domain::execution_protocol::ExecutionProtocolState>> {
        let state = self
            .scope_read
            .facts()
            .state
            .as_ref()
            .context("idle read State missing")?;
        let Some(binding) = &state.current_execution_binding else {
            return Ok(None);
        };
        let Some(activation) = binding.activation_id.as_deref() else {
            return Ok(None);
        };
        let mut request = self.input_request();
        request.insert_admitted_root(Family::Attempt, activation)?;
        let inputs = self.inputs(&request)?;
        if !inputs.execution.initialized {
            return Ok(None);
        }
        // The predecessor is a selected relation of a qualified current Attempt.
        let mut protocol = inputs.protocol_state()?;
        if let Some(prior) = protocol
            .attempts
            .get(activation)
            .and_then(|attempt| attempt.recovery_of_attempt_id.as_ref())
        {
            request.insert_admitted_root(Family::Attempt, prior)?;
            protocol = self.inputs(&request)?.protocol_state()?;
        }
        Ok(Some(protocol))
    }
    pub(in crate::runtime) fn with_work<T>(
        &self,
        id: &str,
        read: impl FnOnce(&WorkItemRecord) -> Result<T>,
    ) -> Result<Option<T>> {
        resource_io::check_active()?;
        self.scope_read.validate(self.tx, &self.scope)?;
        if let Some(work) = self.work_reads.borrow().get(id) {
            return work.work_item(id)?.map(read).transpose();
        }
        let mut capture = WorkCaptureRequest::current();
        capture.include_live = false;
        capture.nonopen_limit = 0;
        capture
            .mandatory_roots
            .insert(WorkKey::new(WorkFamily::Work, id));
        let request = WorkDomainLoadRequest {
            capture,
            record_limit: self.record_limit,
            byte_limit: self.byte_limit,
        };
        let snapshot =
            preflight_named_work_snapshot_tx(self.tx, &self.scope, &request, &self.scope_read)?
                .require_current()?;
        let work = snapshot
            .resident
            .with_budget(self.runtime.inner.input_evidence_budget.clone())?
            .capture(&request.capture)?;
        resource_io::admit_cloned_value(&id)?;
        resource_io::charge_owned_metadata(128, 3)?;
        let work = Arc::new(work);
        let result = work.work_item(id)?.map(read).transpose();
        self.work_reads.borrow_mut().insert(id.to_owned(), work);
        result
    }
    pub(in crate::runtime) fn validate_task_observation(&self, revision: u64) -> Result<()> {
        resource_io::check_active()?;
        crate::runtime_db::task_snapshot::validate_task_domain_observation_with_scope_tx(
            self.tx,
            &self.scope,
            revision,
            &self.scope_read,
        )
    }

    pub(in crate::runtime) fn prepare_result_sources<'b>(
        &self,
        identities: impl IntoIterator<Item = &'b str>,
    ) -> Result<()> {
        resource_io::check_active()?;
        self.scope_read.validate(self.tx, &self.scope)?;
        let mut missing = BTreeSet::new();
        for id in identities {
            if !self
                .task_reads
                .borrow()
                .keys()
                .any(|key| key.family == TaskFamily::ResultSettlement && key.id == id)
            {
                resource_io::admit_cloned_value(&(TaskFamily::ResultSettlement, id))?;
                missing.insert(TaskKey::new(TaskFamily::ResultSettlement, id));
            }
        }
        self.prepare_tasks(missing)
    }
    fn tasks(&self, key: TaskKey) -> Result<Arc<crate::runtime::resident_task::CapturedTasks>> {
        self.scope_read.validate(self.tx, &self.scope)?;
        if let Some(tasks) = self.task_reads.borrow().get(&key) {
            return Ok(tasks.clone());
        }
        resource_io::admit_cloned_value(&key)?;
        self.prepare_tasks(BTreeSet::from([key.clone()]))?;
        self.task_reads
            .borrow()
            .get(&key)
            .cloned()
            .context("idle Task selection missing")
    }

    fn prepare_tasks(&self, keys: BTreeSet<TaskKey>) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        self.scope_read.validate(self.tx, &self.scope)?;
        let capture = TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            recovery_source_ids: Default::default(),
            required_keys: keys,
        };
        let request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture,
            record_limit: self.record_limit,
            byte_limit: self.byte_limit,
        };
        let snapshot = preflight_named_task_snapshot_tx(
            self.tx,
            &self.scope,
            &request,
            &self.runtime.inner.input_evidence_budget,
            &self.scope_read,
        )?
        .require_current()?;
        let tasks = snapshot
            .resident
            .with_budget(self.runtime.inner.input_evidence_budget.clone())?
            .capture(&request.capture)?;
        resource_io::admit_cloned_value(&request.capture.required_keys)?;
        resource_io::charge_owned_metadata(
            128 * u64::try_from(request.capture.required_keys.len())?,
            3 * request.capture.required_keys.len(),
        )?;
        let tasks = Arc::new(tasks);
        let mut cache = self.task_reads.borrow_mut();
        for key in &request.capture.required_keys {
            cache.insert(key.clone(), tasks.clone());
        }
        Ok(())
    }
    pub(in crate::runtime) fn record(&self, id: &str) -> Result<Option<QualifiedResultRead>> {
        let key = TaskKey::new(TaskFamily::ResultSettlement, id);
        let tasks = self.tasks(key.clone())?;
        let Some(row) = tasks.lookup(&key)? else {
            return Ok(None);
        };
        row.validate(&self.scope)?;
        let CanonicalTaskFact::ResultSettlement(_) = &row.fact else {
            bail!("idle Result has another family");
        };
        Ok(Some(QualifiedResultRead {
            fact: row.clone(),
            _capture: tasks,
        }))
    }
    fn valid_result(&self, record: &TaskResultSettlementRecord) -> Result<bool> {
        let tasks = self.tasks(TaskKey::new(
            TaskFamily::ResultSettlement,
            &record.result_identity,
        ))?;
        Ok(matches!(
            tasks.result_dependency(&record.result_identity)?.relation,
            crate::runtime::resident_task::TaskResultDependencyRelation::QualifiedCurrent { .. }
        ))
    }
    pub(in crate::runtime) fn latest_for_message(
        &self,
        id: &str,
    ) -> Result<Option<QualifiedResultRead>> {
        let ids = self.ids(
            "SELECT result_identity AS id FROM task_result_settlements WHERE message_id=?1",
            [id],
        )?;
        ids.first()
            .map(|id| self.record(id))
            .transpose()
            .map(Option::flatten)
    }
    pub(in crate::runtime) fn ids<P: rusqlite::Params + Clone>(
        &self,
        sql: &str,
        params: P,
    ) -> Result<Vec<String>> {
        let (count, bytes): (usize, u64) = self.tx.query_row(
            &format!(
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(id AS BLOB))),0) FROM ({sql}) selected"
            ),
            params.clone(),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        anyhow::ensure!(
            count <= self.record_limit,
            "idle selector record limit exceeded"
        );
        resource_io::charge_owned_metadata(
            bytes
                .checked_add((count as u64) * 128)
                .context("idle selector overflow")?,
            count,
        )?;
        self.tx
            .prepare(sql)?
            .query_map(params, |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()
            .map_err(Into::into)
    }
    fn records<P: rusqlite::Params + Clone>(
        &self,
        sql: &str,
        params: P,
    ) -> Result<Vec<QualifiedResultRead>> {
        let ids = self.ids(sql, params)?;
        // One bounded selection must not repeatedly decode the scope,
        // namespace metadata and shared Task dependencies for every result.
        resource_io::admit_cloned_value(&ids)?;
        resource_io::charge_owned_metadata(128 * u64::try_from(ids.len())?, ids.len())?;
        let missing = ids
            .iter()
            .filter(|id| {
                !self
                    .task_reads
                    .borrow()
                    .keys()
                    .any(|key| key.family == TaskFamily::ResultSettlement && key.id == **id)
            })
            .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id))
            .collect();
        self.prepare_tasks(missing)?;
        resource_io::charge_owned_metadata(
            u64::try_from(ids.len() * std::mem::size_of::<QualifiedResultRead>())?,
            ids.len(),
        )?;
        ids.iter()
            .map(|id| self.record(id)?.context("idle selected Result missing"))
            .collect()
    }
    pub(in crate::runtime) fn admitted(
        &self,
        activation: &str,
    ) -> Result<Vec<QualifiedResultRead>> {
        self.records("SELECT result_identity AS id FROM task_result_settlements WHERE agent_id=?1 AND activation_id=?2
            AND state='caller_admitted' ORDER BY created_at,result_identity LIMIT 65", params![self.scope.agent_id, activation])
    }
    pub(in crate::runtime) fn unsettled(
        &self,
        owner: Option<&str>,
    ) -> Result<Vec<QualifiedResultRead>> {
        self.records(
            &results::OWNER_SQL.replacen("SELECT payload_json", "SELECT result_identity AS id", 1),
            params![self.scope.agent_id, owner, INPUT_BATCH_SCAN as i64],
        )
    }
    pub(in crate::runtime) fn reply_candidates(
        &self,
        owner: Option<&str>,
        message: &str,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<QualifiedResultRead>> {
        let records = self.records(
            &results::REPLY_SQL.replacen(
                "SELECT settlements.payload_json",
                "SELECT settlements.result_identity AS id",
                1,
            ),
            params![self.scope.agent_id, owner, message],
        )?;
        let mut eligible = Vec::new();
        for record in records {
            if record.created_at > cutoff || !self.valid_result(&record)? {
                continue;
            }
            let matches = self
                .fact(Family::Message, &record.message_id)?
                .map(|row| {
                    let Fact::Message(value) = &row.record else {
                        bail!("idle Message has another family");
                    };
                    Ok(value.agent_id == self.scope.agent_id
                        && value.task_id.as_deref() == Some(record.task_id.as_str())
                        && crate::wake_contract::agent_message_reply_reference(value)
                            == Some(message))
                })
                .transpose()?
                .unwrap_or(false);
            if matches {
                eligible.push(record);
            }
        }
        Ok(eligible)
    }
    pub(in crate::runtime) fn queue(&self, id: &str) -> Result<Option<QualifiedQueueRead>> {
        resource_io::admit_cloned_value(&id)?;
        let selection = crate::runtime_db::current_query::CurrentSelection {
            queue_ids: BTreeSet::from([id.to_owned()]),
            events_after: None,
        };
        let query = crate::runtime_db::current_query::prepare_with_scope_read(
            self.tx,
            &self.scope,
            Some(&selection),
            &self.runtime.inner.input_evidence_budget,
            (self.byte_limit, self.record_limit),
            &self.scope_read,
        )?;
        query.validate_binding(&self.scope, &self.runtime.inner.input_evidence_budget)?;
        query
            .queue_record(id)?
            .map(|body| {
                anyhow::ensure!(
                    matches!(
                        body.record(),
                        crate::runtime_db::current_read_writes::CurrentRecord::Queue(_)
                    ),
                    "idle Queue has another family"
                );
                Ok(QualifiedQueueRead(body.clone()))
            })
            .transpose()
    }
    pub(in crate::runtime) fn delivery(
        &self,
        id: &str,
    ) -> Result<Option<crate::types::AgentMessageDeliveryRecord>> {
        self.json_row("agent_message_deliveries", "message_id", id)
    }
    fn json_row<T: serde::de::DeserializeOwned>(
        &self,
        table: &str,
        key: &str,
        id: &str,
    ) -> Result<Option<T>> {
        let bytes: Option<u64> = self.tx.query_row(&format!("SELECT length(CAST(payload_json AS BLOB)) FROM {table} WHERE {key}=?1 ORDER BY rowid DESC LIMIT 1"),
            [id], |row| row.get(0)).optional()?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        resource_io::charge_owned_payload(
            bytes.checked_mul(8).context("idle source row overflow")?,
        )?;
        resource_io::charge_owned_metadata(256, 1)?;
        let payload: String = self.tx.query_row(
            &format!("SELECT payload_json FROM {table} WHERE {key}=?1 ORDER BY rowid DESC LIMIT 1"),
            [id],
            |row| row.get(0),
        )?;
        Ok(Some(serde_json::from_str(&payload)?))
    }
    pub(in crate::runtime) fn source_matches(
        &self,
        record: &TaskResultSettlementRecord,
        reply: bool,
    ) -> Result<bool> {
        self.source_predicate(record, |tx, record| {
            if reply {
                results::reply_source_is_consumed_for_owner_tx(tx, record)
            } else {
                results::non_reply_source_matches_owner_tx(tx, record)
            }
        })
    }

    pub(in crate::runtime) fn reply_source_recoverable(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        self.source_predicate(record, results::reply_source_is_recoverable_tx)
    }

    pub(in crate::runtime) fn reply_source_consumed(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        self.source_predicate(record, results::reply_source_is_consumed_tx)
    }

    fn source_predicate(
        &self,
        record: &TaskResultSettlementRecord,
        predicate: impl FnOnce(&Transaction<'_>, &TaskResultSettlementRecord) -> Result<bool>,
    ) -> Result<bool> {
        if !self.valid_result(record)? {
            return Ok(false);
        }
        let Some(message_row) = self.fact(Family::Message, &record.message_id)? else {
            return Ok(false);
        };
        let Fact::Message(message) = &message_row.record else {
            bail!("idle Message has another family");
        };
        let mut bytes = crate::runtime::input_evidence_budget::serialized_bytes(&message)?;
        if let Some(id) = crate::wake_contract::agent_message_reply_reference(&message) {
            let Some(source_row) = self.fact(Family::Message, id)? else {
                return Ok(false);
            };
            let Fact::Message(source) = &source_row.record else {
                bail!("idle reply Message has another family");
            };
            bytes += crate::runtime::input_evidence_budget::serialized_bytes(&source)?;
            if let Some(delivery) = self.delivery(id)? {
                bytes += crate::runtime::input_evidence_budget::serialized_bytes(&delivery)?;
            }
            // Observe the same exact owner/Task waiter relation before its source predicate.
            let mut capture = WorkCaptureRequest::current();
            capture.include_live = false;
            capture.nonopen_limit = 0;
            capture.required_task_ids.insert(record.task_id.clone());
            capture.required_trigger_message_ids.insert(id.to_owned());
            let request = WorkDomainLoadRequest {
                capture,
                record_limit: self.record_limit,
                byte_limit: self.byte_limit,
            };
            let snapshot =
                preflight_named_work_snapshot_tx(self.tx, &self.scope, &request, &self.scope_read)?
                    .require_current()?;
            let _work = snapshot
                .resident
                .with_budget(self.runtime.inner.input_evidence_budget.clone())?
                .capture(&request.capture)?;
            for id in self.ids("SELECT turn_id AS id FROM turn_records WHERE agent_id=?1 AND terminal_kind IN ('completed','interrupted')
                AND (trigger_message_id=?2 OR EXISTS(SELECT 1 FROM json_each(payload_json,'$.input_message_ids') input WHERE input.value=?2))
                ORDER BY turn_index DESC LIMIT 64", params![self.scope.agent_id,id])? { self.shared_turn(&id)?; }
        }
        let task: Option<usize> = self
            .tx
            .query_row(
                "SELECT length(CAST(payload_json AS BLOB)) FROM tasks WHERE task_id=?1",
                [&record.task_id],
                |row| row.get(0),
            )
            .optional()?;
        resource_io::charge_owned_payload(
            bytes
                .checked_add(task.unwrap_or(0) as u64)
                .and_then(|n| n.checked_mul(16))
                .context("idle predicate material overflow")?,
        )?;
        predicate(self.tx, record)
    }
    pub(in crate::runtime) fn source_turn(&self, message: &str) -> Result<Option<TurnRecord>> {
        let ids = self.ids("SELECT turn_id AS id FROM turn_records WHERE agent_id=?1 AND trigger_message_id=?2
            AND json_extract(payload_json,'$.idle_input_batch.anchor_message_id')=?2 ORDER BY turn_index DESC LIMIT 1",
            params![self.scope.agent_id, message])?;
        ids.first()
            .map(|id| self.turn(id))
            .transpose()
            .map(Option::flatten)
    }
    pub(in crate::runtime) fn sender_has_wait(&self, message: &str) -> Result<bool> {
        let mut capture = WorkCaptureRequest::current();
        capture.nonopen_limit = 0;
        capture
            .required_trigger_message_ids
            .insert(message.to_owned());
        let request = WorkDomainLoadRequest {
            capture,
            record_limit: self.record_limit,
            byte_limit: self.byte_limit,
        };
        let snapshot =
            preflight_named_work_snapshot_tx(self.tx, &self.scope, &request, &self.scope_read)?
                .require_current()?;
        let work = snapshot
            .resident
            .with_budget(self.runtime.inner.input_evidence_budget.clone())?
            .capture(&request.capture)?;
        Ok(work.records.iter().any(|row| {
            matches!(&row.record,
            crate::runtime::resident_work::CanonicalWorkRecord::Wait(wait)
                if wait.status == crate::types::WaitConditionStatus::Active
                    || wait.trigger_message_id.as_deref() == Some(message))
        }))
    }
    #[cfg(test)]
    pub(in crate::runtime) fn pending_bytes(
        &self,
        anchor: &MessageEnvelope,
        records: &[impl std::borrow::Borrow<TaskResultSettlementRecord>],
    ) -> Result<usize> {
        self.scope_read.validate(self.tx, &self.scope)?;
        let mut previews = self.pending_previews.borrow_mut();
        for record in records {
            let record = record.borrow();
            if let Some((original, _)) = previews.get(&record.result_identity) {
                anyhow::ensure!(
                    &**original == record,
                    "idle pending Result changed within read cut"
                );
                continue;
            }
            let original = self
                .record(&record.result_identity)?
                .context("idle pending Result missing")?;
            anyhow::ensure!(
                &*original == record,
                "idle pending Result changed within read cut"
            );
            let message = self.fact(Family::Message, &record.message_id)?;
            let message = message
                .as_ref()
                .map(|row| match &row.record {
                    Fact::Message(message) => Ok(message),
                    _ => bail!("idle pending Message has another family"),
                })
                .transpose()?;
            let reply = match message.and_then(crate::wake_contract::agent_message_reply_reference)
            {
                Some(id) => self.fact(Family::Message, id)?,
                None => None,
            };
            let reply = reply
                .as_ref()
                .map(|row| match &row.record {
                    Fact::Message(message) => Ok(message),
                    _ => bail!("idle pending reply has another family"),
                })
                .transpose()?;
            // Project once from borrowed qualified bodies. Capacity trials only
            // count immutable fragments, never copying the source bodies.
            // This projects strings from borrowed facts, not a JSON decode or
            // an envelope clone. Bound temporary text and the fixed line/header
            // containers before formatting; JSON body shape adds no parser cells.
            let bytes =
                crate::runtime::input_evidence_budget::serialized_bytes(&(record, message, reply))?
                    .checked_mul(4)
                    .context("idle pending preview material overflow")?;
            resource_io::charge_owned_payload(bytes)?;
            // Fixed preview lines plus the temporary header labels. Source
            // strings above and the retained map key/record below are separate.
            let cells = crate::context::PENDING_TASK_RESULT_PREVIEW_LINES + 16;
            resource_io::charge_owned_metadata(
                u64::try_from(
                    std::mem::size_of::<crate::context::PendingTaskResultPreview>()
                        + cells * std::mem::size_of::<String>(),
                )?,
                cells + 1,
            )?;
            resource_io::admit_cloned_value(&record.result_identity)?;
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<QualifiedResultRead>())?,
                1,
            )?;
            let preview = crate::context::pending_task_result_preview(record, message, reply);
            previews.insert(record.result_identity.clone(), (original, preview));
        }
        Ok(crate::context::pending_task_results_preview_bytes(
            records
                .iter()
                .map(|record| &previews[&record.borrow().result_identity].1),
            anchor,
        ))
    }
}

impl IdleRead<'_, '_> {
    fn qualify_selected(
        &self,
        records: Vec<TaskResultSettlementRecord>,
    ) -> Result<Vec<QualifiedResultRead>> {
        resource_io::admit_cloned_value(&records)?;
        self.prepare_result_sources(records.iter().map(|record| record.result_identity.as_str()))?;
        records
            .iter()
            .map(|selected| {
                let qualified = self
                    .record(&selected.result_identity)?
                    .context("idle selected Result missing")?;
                anyhow::ensure!(
                    &*qualified == selected,
                    "idle selected Result changed within read cut"
                );
                Ok(qualified)
            })
            .collect()
    }

    pub(in crate::runtime) fn exact_waiter(
        &self,
        owner: Option<&str>,
        activation: &str,
    ) -> Result<Option<QualifiedResultRead>> {
        let selected = results::exact_waiter_recovery_candidate_tx(
            self.tx,
            &self.scope.agent_id,
            owner,
            activation,
        )?;
        Ok(self.qualify_selected(selected.into_iter().collect())?.pop())
    }

    pub(in crate::runtime) fn recovery_candidates(
        &self,
        owner: Option<&str>,
        activation: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<QualifiedResultRead>> {
        self.qualify_selected(results::recovery_snapshot_candidates_for_owner_tx(
            self.tx,
            &self.scope.agent_id,
            owner,
            activation,
            at,
            INPUT_BATCH_SCAN,
        )?)
    }

    pub(in crate::runtime) fn filter_recovery_candidates(
        &self,
        activation: &str,
        records: &[QualifiedResultRead],
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<QualifiedResultRead>> {
        let records = self.owned_records(records.to_vec())?;
        self.qualify_selected(results::filter_recovery_snapshot_candidates_tx(
            self.tx,
            &self.scope.agent_id,
            activation,
            &records,
            at,
        )?)
    }

    pub(in crate::runtime) fn same_turn_candidates(
        &self,
        turn: &str,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<QualifiedResultRead>> {
        self.qualify_selected(results::same_turn_candidates_tx(
            self.tx,
            &self.scope.agent_id,
            turn,
            cutoff,
            32,
        )?)
    }

    pub(in crate::runtime) fn interrupted_result_anchor(
        &self,
        message: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<String>> {
        if let Some(record) = self.latest_for_message(message)? {
            self.shared_message(&record.message_id)?;
            self.prepare_result_sources(std::iter::once(record.result_identity.as_str()))?;
        }
        results::interrupted_admitted_result_anchor_tx(self.tx, &self.scope.agent_id, message, at)
    }

    pub(in crate::runtime) fn owned_records(
        &self,
        records: Vec<QualifiedResultRead>,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        records
            .iter()
            .map(|record| {
                resource_io::admit_cloned_value(&**record)?;
                Ok((**record).clone())
            })
            .collect()
    }

    /// Trials may propose projections, but never reopen source bodies.
    pub(in crate::runtime) fn pending_text(
        &self,
        anchor: &MessageEnvelope,
        records: &[impl std::borrow::Borrow<TaskResultSettlementRecord>],
    ) -> Result<Option<String>> {
        let mut identities = BTreeSet::new();
        let mut previews = Vec::new();
        for record in records {
            let record = record.borrow();
            if record.message_id == anchor.id || !identities.insert(&record.result_identity) {
                continue;
            }
            let original = self
                .record(&record.result_identity)?
                .context("idle pending Result missing")?;
            anyhow::ensure!(
                original.message_id == record.message_id
                    && original.task_id == record.task_id
                    && original.parent_turn_id == record.parent_turn_id
                    && original.rejoin_generation == record.rejoin_generation,
                "idle proposed projection differs from its qualified Result"
            );
            let message = self.shared_message(&record.message_id)?;
            let reply = message
                .as_deref()
                .and_then(crate::wake_contract::agent_message_reply_reference)
                .map(|id| self.shared_message(id))
                .transpose()?
                .flatten();
            let bytes = crate::runtime::input_evidence_budget::serialized_bytes(&(
                record,
                message.as_deref(),
                reply.as_deref(),
            ))?
            .checked_mul(8)
            .context("idle pending projection material overflow")?;
            resource_io::charge_owned_payload(bytes)?;
            resource_io::charge_owned_metadata(4096, 64)?;
            previews.push(crate::context::pending_task_result_preview(
                record,
                message.as_deref(),
                reply.as_deref(),
            ));
        }
        Ok(crate::context::render_pending_task_result_previews(
            previews.iter(),
            anchor,
        ))
    }
}

impl RuntimeHandle {
    pub(in crate::runtime) fn with_idle_read<T>(
        &self,
        expected: Option<&AgentState>,
        material: &ResourceReadMaterial,
        prepare: impl FnOnce(&IdleRead<'_, '_>) -> Result<T>,
    ) -> Result<T> {
        let mut prepare = Some(prepare);
        material.read(|| {
            self.inner.runtime_db.transaction(|tx| {
                prepare.take().context("idle selection already executed")?(&IdleRead::new(
                    self, tx, expected,
                )?)
            })
        })
    }
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
