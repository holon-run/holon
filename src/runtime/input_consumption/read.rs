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

struct PendingResultPreview {
    original: QualifiedResultRead,
    projections_digest: [u8; 32],
    preview: crate::context::PendingTaskResultPreview,
}

#[derive(Clone, Copy)]
pub(in crate::runtime) struct PendingResultView<'a> {
    pub record: &'a TaskResultSettlementRecord,
    pub source_projection: Option<&'a IdleInputBatchItem>,
    pub reply_projection: Option<&'a IdleInputBatchItem>,
}

impl<'a> From<&'a TaskResultSettlementRecord> for PendingResultView<'a> {
    fn from(record: &'a TaskResultSettlementRecord) -> Self {
        Self {
            record,
            source_projection: record.source_projection.as_ref(),
            reply_projection: record.reply_projection.as_ref(),
        }
    }
}

/// Cache presentation equality without retaining another pair of body copies.
fn pending_projections_digest(
    source: Option<&IdleInputBatchItem>,
    reply: Option<&IdleInputBatchItem>,
) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    struct DigestWriter(Sha256);
    impl std::io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, &(source, reply))?;
    Ok(writer.0.finalize().into())
}

/// Borrow the complete key-set shape before allocating its owning entries.
struct ResultSourceKeys<I>(I);

impl<'a, I> serde::Serialize for ResultSourceKeys<I>
where
    I: Iterator<Item = &'a str> + Clone,
{
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(None)?;
        for id in self.0.clone() {
            sequence.serialize_element(&(TaskFamily::ResultSettlement, id))?;
        }
        sequence.end()
    }
}

fn same_pending_result(
    original: &TaskResultSettlementRecord,
    trial: &TaskResultSettlementRecord,
) -> bool {
    // Exhaustive: new canonical fields must join this borrowed comparison.
    let TaskResultSettlementRecord {
        result_identity,
        agent_id,
        task_id,
        message_id,
        work_item_id,
        rejoin_generation,
        source,
        task_status,
        state,
        activation_id,
        disposition,
        created_at,
        updated_at,
        admitted_at,
        settled_at,
        deferred_reason,
        deferred_at,
        next_recheck_at,
        recovery_epoch,
        recovery_attempts,
        last_recovery_root_attempt_id,
        last_recovery_admitted_at,
        recovery_paused_at,
        recovery_diagnostic_at,
        source_projection: _,
        reply_projection: _,
    } = trial;
    &original.result_identity == result_identity
        && &original.agent_id == agent_id
        && &original.task_id == task_id
        && &original.message_id == message_id
        && &original.work_item_id == work_item_id
        && &original.rejoin_generation == rejoin_generation
        && &original.source == source
        && &original.task_status == task_status
        && &original.state == state
        && &original.activation_id == activation_id
        && &original.disposition == disposition
        && &original.created_at == created_at
        && &original.updated_at == updated_at
        && &original.admitted_at == admitted_at
        && &original.settled_at == settled_at
        && &original.deferred_reason == deferred_reason
        && &original.deferred_at == deferred_at
        && &original.next_recheck_at == next_recheck_at
        && &original.recovery_epoch == recovery_epoch
        && &original.recovery_attempts == recovery_attempts
        && &original.last_recovery_root_attempt_id == last_recovery_root_attempt_id
        && &original.last_recovery_admitted_at == last_recovery_admitted_at
        && &original.recovery_paused_at == recovery_paused_at
        && &original.recovery_diagnostic_at == recovery_diagnostic_at
}

#[derive(Debug, Clone)]
pub(in crate::runtime) struct QualifiedMessageRead {
    fact: Arc<crate::runtime::resident_input_evidence::QualifiedInputEvidence>,
    _capture: Arc<crate::runtime::resident_input_evidence::CapturedInputEvidence>,
}

impl QualifiedMessageRead {
    fn into_source_loan(self) -> results::TaskResultSourceLoan {
        results::TaskResultSourceLoan::new(self.fact, self._capture)
    }
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
    fn into_source_loan(self) -> results::TaskResultSourceLoan {
        results::TaskResultSourceLoan::new(self.fact, self._capture)
    }

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
    task_material: ResourceReadMaterial,
    source_read: results::TaskResultSourceRead<'a, 'connection>,
    input_reads: RefCell<
        BTreeMap<
            BTreeSet<Key>,
            Arc<crate::runtime::resident_input_evidence::CapturedInputEvidence>,
        >,
    >,
    task_reads: RefCell<BTreeMap<TaskKey, Arc<crate::runtime::resident_task::CapturedTasks>>>,
    work_reads: RefCell<BTreeMap<String, Arc<crate::runtime::resident_work::CapturedWork>>>,
    reply_work_read: RefCell<Option<crate::runtime::resident_work::CapturedWork>>,
    pending_previews: RefCell<BTreeMap<String, PendingResultPreview>>,
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
        let task_material =
            ResourceReadMaterial::current_or_new(&runtime.inner.input_evidence_budget, &scope)?;
        Ok(Self {
            runtime,
            tx,
            scope,
            scope_read: facts,
            task_material,
            source_read: results::TaskResultSourceRead::new(
                tx,
                runtime.inner.mutation_owner.agent_id(),
            ),
            input_reads: RefCell::new(BTreeMap::new()),
            task_reads: RefCell::new(BTreeMap::new()),
            work_reads: RefCell::new(BTreeMap::new()),
            reply_work_read: RefCell::new(None),
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

    pub(in crate::runtime) fn prepare_result_sources<'b, I>(&self, identities: I) -> Result<()>
    where
        I: IntoIterator<Item = &'b str>,
        I::IntoIter: Clone,
    {
        resource_io::check_active()?;
        self.scope_read.validate(self.tx, &self.scope)?;
        let missing = identities.into_iter().filter(|id| {
            !self
                .task_reads
                .borrow()
                .keys()
                .any(|key| key.family == TaskFamily::ResultSettlement && key.id == *id)
        });
        if missing.clone().next().is_none() {
            return Ok(());
        }
        resource_io::admit_cloned_value(&ResultSourceKeys(missing.clone()))?;
        self.prepare_tasks(
            missing
                .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id))
                .collect(),
        )
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
        self.source_predicate(record, |source, record| {
            source.source_matches(record, reply)
        })
    }

    pub(in crate::runtime) fn reply_source_recoverable(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        self.source_predicate(record, |source, record| {
            source.reply_source_recoverable(record)
        })
    }

    pub(in crate::runtime) fn reply_source_available_for_exact_wait(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        self.source_predicate(record, |source, record| {
            if source.reply_source_consumed(record)? {
                return Ok(true);
            }
            let Some(result) = self.shared_message(&record.message_id)? else {
                return Ok(false);
            };
            let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(&result)
            else {
                return Ok(false);
            };
            let Some(queue) = self.queue(reply_id)? else {
                return Ok(false);
            };
            source.dropped_reply_source_matches(record, &queue)
        })
    }

    pub(in crate::runtime) fn source_revision_repair_retains_source(
        &self,
        message_id: &str,
    ) -> Result<bool> {
        self.scope_read.validate(self.tx, &self.scope)?;
        // Resolution clears the fault, not this incarnation's consumption fence.
        let retained = self.tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM source_revision_repair_block_sources
                 WHERE agent_id = ?1 AND incarnation = ?2 AND message_id = ?3
             )",
            params![
                self.scope.agent_id,
                self.scope.identity_incarnation,
                message_id
            ],
            |row| row.get(0),
        )?;
        self.scope_read.validate(self.tx, &self.scope)?;
        Ok(retained)
    }

    pub(in crate::runtime) fn prepare_reply_source_work(
        &self,
        records: &[QualifiedResultRead],
    ) -> Result<()> {
        self.prepare_reply_source_work_records(records.iter().map(|record| &**record))
    }

    pub(in crate::runtime) fn prepare_reply_source_work_records<'b, I>(
        &self,
        records: I,
    ) -> Result<()>
    where
        I: IntoIterator<Item = &'b TaskResultSettlementRecord>,
        I::IntoIter: Clone,
    {
        resource_io::check_active()?;
        self.scope_read.validate(self.tx, &self.scope)?;
        let records = records.into_iter();
        // This scan already visits every selected Message. Prepare their
        // uncovered roots together instead of one capture per Result.
        let mut request = self.input_request();
        request.extend_admitted_roots(|| {
            records
                .clone()
                .filter(|record| {
                    !self.input_reads.borrow().values().any(|capture| {
                        capture.records.iter().any(|row| {
                            row.locator.key.family == Family::Message
                                && row.locator.key.id == record.message_id
                        }) || capture
                            .unavailable
                            .iter()
                            .any(|key| key.family == Family::Message && key.id == record.message_id)
                    })
                })
                .map(|record| (Family::Message, record.message_id.as_str()))
        })?;
        if !request.mandatory_roots.is_empty() {
            self.inputs(&request)?;
        }
        let mut capture = WorkCaptureRequest::current();
        capture.include_live = false;
        capture.nonopen_limit = 0;
        for record in records {
            let Some(message) = self.shared_message(&record.message_id)? else {
                continue;
            };
            let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(&message)
            else {
                continue;
            };
            if !self.valid_result(record)? {
                continue;
            }
            if !capture.required_task_ids.contains(&record.task_id) {
                resource_io::admit_cloned_value(&record.task_id)?;
                capture.required_task_ids.insert(record.task_id.clone());
            }
            if !capture.required_trigger_message_ids.contains(reply_id) {
                resource_io::admit_cloned_value(&reply_id)?;
                capture
                    .required_trigger_message_ids
                    .insert(reply_id.to_owned());
            }
        }
        if !capture.required_task_ids.is_empty() {
            self.prepare_reply_work(capture)?;
        }
        Ok(())
    }

    fn prepare_reply_work(&self, capture: WorkCaptureRequest) -> Result<()> {
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
        *self.reply_work_read.borrow_mut() = Some(work);
        Ok(())
    }

    fn reply_work_covers(&self, task: &str, reply: &str) -> Result<bool> {
        resource_io::check_active()?;
        self.scope_read.validate(self.tx, &self.scope)?;
        Ok(self.reply_work_read.borrow().as_ref().is_some_and(|work| {
            work.scope == self.scope
                && work.task_wait_keys.contains_key(task)
                && work.trigger_wait_keys.contains_key(reply)
        }))
    }

    fn source_predicate(
        &self,
        record: &TaskResultSettlementRecord,
        predicate: impl FnOnce(
            &results::TaskResultSourceRead<'_, '_>,
            &TaskResultSettlementRecord,
        ) -> Result<bool>,
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
        // Reply Message/Turn bodies are borrowed through the resolver below.
        // Non-reply predicates still own their independent Message decode.
        let mut bytes = 0;
        if let Some(id) = crate::wake_contract::agent_message_reply_reference(&message) {
            let Some(source_row) = self.fact(Family::Message, id)? else {
                return Ok(false);
            };
            let Fact::Message(_) = &source_row.record else {
                bail!("idle reply Message has another family");
            };
            // Observe the same exact owner/Task waiter relation before its source predicate.
            if !self.reply_work_covers(&record.task_id, id)? {
                let mut capture = WorkCaptureRequest::current();
                capture.include_live = false;
                capture.nonopen_limit = 0;
                resource_io::admit_cloned_value(&(&record.task_id, id))?;
                capture.required_task_ids.insert(record.task_id.clone());
                capture.required_trigger_message_ids.insert(id.to_owned());
                self.prepare_reply_work(capture)?;
            }
        } else {
            bytes = crate::runtime::input_evidence_budget::serialized_bytes(&message)?;
        }
        // The source reader admits Task bytes only when it actually decodes
        // the current row; same-cut metadata loans do not create another body.
        resource_io::charge_owned_payload(
            bytes
                .checked_mul(16)
                .context("idle predicate material overflow")?,
        )?;
        self.source_read
            .validate_cut(self.tx, &self.scope.agent_id)?;
        self.with_source_resolver(|resolver| {
            let source = self.source_read.with_resolver(resolver)?;
            predicate(&source, record)
        })
    }

    fn with_source_resolver<T>(
        &self,
        read: impl FnOnce(&results::TaskResultSourceResolver<'_, 'connection>) -> Result<T>,
    ) -> Result<T> {
        let resolve = |family, id: &str| match family {
            Family::Message => self
                .shared_message(id)
                .map(|message| message.map(QualifiedMessageRead::into_source_loan)),
            Family::Turn => self
                .shared_turn(id)
                .map(|turn| turn.map(QualifiedTurnRead::into_source_loan)),
            _ => bail!("source loan only accepts Message or Turn"),
        };
        let resolve_task = |id: &str| {
            self.scope_read.validate(self.tx, &self.scope)?;
            self.task_material.finish()?;
            for captured in self.task_reads.borrow().values() {
                if let Some(fact) = captured.records.iter().find(|fact| {
                    fact.locator.key.family == TaskFamily::Task && fact.locator.key.id == id
                }) {
                    return Ok(Some(results::TaskResultTaskLoan::new(
                        self.tx,
                        fact.clone(),
                        captured.clone(),
                        self.task_material.clone(),
                    )));
                }
            }
            Ok(None)
        };
        let resolver = results::TaskResultSourceResolver::with_scope_read(
            self.tx,
            &self.scope,
            &self.runtime.inner.input_evidence_budget,
            &self.scope_read,
            &resolve,
        )?
        .with_task_source(&resolve_task);
        read(&resolver)
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
    fn prepare_pending_previews<'records>(
        &self,
        records: impl Iterator<Item = PendingResultView<'records>> + Clone,
    ) -> Result<()> {
        resource_io::check_active()?;
        self.scope_read.validate(self.tx, &self.scope)?;
        let mut previews = self.pending_previews.borrow_mut();
        for (index, view) in records.clone().enumerate() {
            let record = view.record;
            if records
                .clone()
                .take(index)
                .any(|prior| prior.record.result_identity == record.result_identity)
            {
                continue;
            }
            let original = match previews.get(&record.result_identity) {
                Some(cached) => cached.original.clone(),
                None => self
                    .record(&record.result_identity)?
                    .context("idle pending Result missing")?,
            };
            anyhow::ensure!(
                same_pending_result(&original, record),
                "idle pending Result changed within read cut"
            );
            let projections_digest =
                pending_projections_digest(view.source_projection, view.reply_projection)?;
            if previews
                .get(&record.result_identity)
                .is_some_and(|cached| cached.projections_digest == projections_digest)
            {
                continue;
            }
            let preview = self.pending_preview_with_projections(
                record,
                view.source_projection,
                view.reply_projection,
            )?;
            resource_io::admit_cloned_value(&record.result_identity)?;
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<PendingResultPreview>())?,
                1,
            )?;
            previews.insert(
                record.result_identity.clone(),
                PendingResultPreview {
                    original,
                    projections_digest,
                    preview,
                },
            );
        }
        Ok(())
    }

    fn pending_preview_with_projections(
        &self,
        record: &TaskResultSettlementRecord,
        source_projection: Option<&IdleInputBatchItem>,
        reply_projection: Option<&IdleInputBatchItem>,
    ) -> Result<crate::context::PendingTaskResultPreview> {
        // Projected previews use only the already qualified Result and sidecars.
        // Do not acquire an unused owning Message closure in that branch.
        let message = if source_projection.is_none() {
            self.fact(Family::Message, &record.message_id)?
        } else {
            None
        };
        let message = message
            .as_ref()
            .map(|row| match &row.record {
                Fact::Message(message) => Ok(message),
                _ => bail!("idle pending Message has another family"),
            })
            .transpose()?;
        let reply = match message.and_then(crate::wake_contract::agent_message_reply_reference) {
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
        let bytes = crate::runtime::input_evidence_budget::serialized_bytes(&(
            record,
            source_projection,
            reply_projection,
            message,
            reply,
        ))?
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
        Ok(
            crate::context::pending_task_result_preview_with_projections(
                record,
                message,
                reply,
                source_projection,
                reply_projection,
            ),
        )
    }

    #[cfg(test)]
    pub(in crate::runtime) fn pending_bytes(
        &self,
        anchor: &MessageEnvelope,
        records: &[impl std::borrow::Borrow<TaskResultSettlementRecord>],
    ) -> Result<usize> {
        Ok(self.pending_measure(anchor, records)?.0)
    }

    /// Trial capacity without allocating aggregate text or dedup containers.
    pub(in crate::runtime) fn pending_measure(
        &self,
        anchor: &MessageEnvelope,
        records: &[impl std::borrow::Borrow<TaskResultSettlementRecord>],
    ) -> Result<(usize, usize)> {
        self.pending_measure_with_candidate(anchor, records, None)
    }

    pub(in crate::runtime) fn pending_measure_with_candidate(
        &self,
        anchor: &MessageEnvelope,
        records: &[impl std::borrow::Borrow<TaskResultSettlementRecord>],
        candidate: Option<&TaskResultSettlementRecord>,
    ) -> Result<(usize, usize)> {
        let records = records
            .iter()
            .map(std::borrow::Borrow::borrow)
            .chain(candidate)
            .map(PendingResultView::from);
        self.pending_measure_views(anchor, records)
    }

    pub(in crate::runtime) fn pending_measure_views<'records>(
        &self,
        anchor: &MessageEnvelope,
        records: impl Iterator<Item = PendingResultView<'records>> + Clone,
    ) -> Result<(usize, usize)> {
        self.prepare_pending_previews(records.clone())?;
        let previews = self.pending_previews.borrow();
        let (bytes, chars) = crate::context::pending_task_results_preview_measure(
            records
                .filter(|view| view.record.message_id != anchor.id)
                .map(|view| &previews[&view.record.result_identity].preview),
            anchor,
        );
        let tokens = if bytes == 0 {
            0
        } else {
            chars
                .saturating_add("[pending_task_results]\n".len())
                .div_ceil(4)
                .max(1)
        };
        Ok((bytes, tokens))
    }

    #[cfg(test)]
    pub(in crate::runtime) fn pending_measure_with_projected_candidate(
        &self,
        anchor: &MessageEnvelope,
        records: &[impl std::borrow::Borrow<TaskResultSettlementRecord>],
        candidate: &TaskResultSettlementRecord,
        source_projection: &IdleInputBatchItem,
        reply_projection: Option<&IdleInputBatchItem>,
    ) -> Result<(usize, usize)> {
        self.pending_measure_with_projected_views(
            anchor,
            records
                .iter()
                .map(std::borrow::Borrow::borrow)
                .map(PendingResultView::from),
            candidate,
            source_projection.into(),
            reply_projection.map(projection::ProjectionView::from),
        )
    }

    /// Both selected sidecars and capacity trials borrow their canonical Result.
    pub(in crate::runtime) fn pending_measure_with_projected_views<'records>(
        &self,
        anchor: &MessageEnvelope,
        records: impl Iterator<Item = PendingResultView<'records>> + Clone,
        candidate: &TaskResultSettlementRecord,
        source_projection: projection::ProjectionView<'_>,
        reply_projection: Option<projection::ProjectionView<'_>>,
    ) -> Result<(usize, usize)> {
        self.prepare_pending_previews(records.clone())?;
        let original = self
            .record(&candidate.result_identity)?
            .context("idle pending Result missing")?;
        anyhow::ensure!(
            same_pending_result(&original, candidate),
            "idle pending Result changed within read cut"
        );
        let previews = self.pending_previews.borrow();
        let pending = records
            .filter(|view| view.record.message_id != anchor.id)
            .map(|view| &previews[&view.record.result_identity].preview);
        let (bytes, chars) = if candidate.message_id == anchor.id {
            crate::context::pending_task_results_preview_measure(pending, anchor)
        } else {
            crate::context::pending_task_results_preview_measure_with_projection_views(
                pending,
                anchor,
                candidate,
                source_projection,
                reply_projection,
            )
        };
        let tokens = if bytes == 0 {
            0
        } else {
            chars
                .saturating_add("[pending_task_results]\n".len())
                .div_ceil(4)
                .max(1)
        };
        Ok((bytes, tokens))
    }
}

impl<'connection> IdleRead<'_, 'connection> {
    fn qualify_selected(
        &self,
        records: Vec<TaskResultSettlementRecord>,
    ) -> Result<Vec<QualifiedResultRead>> {
        resource_io::admit_cloned_value(&records)?;
        self.prepare_result_sources(records.iter().map(|record| record.result_identity.as_str()))?;
        records
            .iter()
            .map(|selected| self.qualify_result(selected))
            .collect()
    }

    pub(in crate::runtime) fn qualify_result(
        &self,
        selected: &TaskResultSettlementRecord,
    ) -> Result<QualifiedResultRead> {
        let qualified = self
            .record(&selected.result_identity)?
            .context("idle selected Result missing")?;
        anyhow::ensure!(
            &*qualified == selected,
            "idle selected Result changed within read cut"
        );
        Ok(qualified)
    }

    pub(in crate::runtime) fn exact_waiter(
        &self,
        owner: Option<&str>,
        activation: &str,
    ) -> Result<Option<QualifiedResultRead>> {
        self.scope_read.validate(self.tx, &self.scope)?;
        self.source_read
            .validate_cut(self.tx, &self.scope.agent_id)?;
        let selected = self.source_read.exact_waiter_candidate(owner, activation)?;
        Ok(self.qualify_selected(selected.into_iter().collect())?.pop())
    }

    pub(in crate::runtime) fn recovery_snapshot(
        &self,
        activation: &str,
    ) -> Result<results::RecoverySnapshotRead<'_, 'connection>> {
        self.scope_read.validate(self.tx, &self.scope)?;
        self.source_read
            .validate_cut(self.tx, &self.scope.agent_id)?;
        self.with_source_resolver(|resolver| {
            results::RecoverySnapshotRead::from_source_read_with_resolver(
                &self.source_read,
                resolver,
                activation,
            )
        })
    }

    pub(in crate::runtime) fn recovery_candidates(
        &self,
        owner: Option<&str>,
        snapshot: &results::RecoverySnapshotRead<'_, '_>,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<QualifiedResultRead>> {
        self.scope_read.validate(self.tx, &self.scope)?;
        snapshot.validate_cut(self.tx, &self.scope.agent_id)?;
        // Select identities before preparing their qualified Task facts. An
        // intermediate decoded Result vector would only be discarded here.
        let identities = snapshot.candidate_identities(owner, at, INPUT_BATCH_SCAN)?;
        self.prepare_result_sources(identities.iter().map(String::as_str))?;
        resource_io::charge_owned_metadata(
            u64::try_from(identities.len() * std::mem::size_of::<QualifiedResultRead>())?,
            identities.len(),
        )?;
        identities
            .iter()
            .map(|id| {
                self.record(id)?
                    .context("idle selected recovery Result missing")
            })
            .collect()
    }

    pub(in crate::runtime) fn filter_recovery_candidates(
        &self,
        snapshot: &results::RecoverySnapshotRead<'_, '_>,
        records: &[QualifiedResultRead],
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<QualifiedResultRead>> {
        self.scope_read.validate(self.tx, &self.scope)?;
        snapshot.validate_cut(self.tx, &self.scope.agent_id)?;
        resource_io::charge_owned_metadata(
            u64::try_from(
                records.len()
                    * (std::mem::size_of::<std::borrow::Cow<'_, TaskResultSettlementRecord>>()
                        + std::mem::size_of::<QualifiedResultRead>()),
            )?,
            records.len() * 2,
        )?;
        let selected = snapshot.filter(records, at)?;
        for record in &selected {
            if let std::borrow::Cow::Owned(record) = record {
                resource_io::admit_cloned_value(record)?;
            }
        }
        self.prepare_result_sources(
            selected
                .iter()
                .map(|record| record.result_identity.as_str()),
        )?;
        selected
            .iter()
            .map(|selected| self.qualify_result(selected))
            .collect()
    }

    pub(in crate::runtime) fn same_turn_candidate_ids(
        &self,
        turn: &str,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<String>> {
        resource_io::check_active()?;
        self.scope_read.validate(self.tx, &self.scope)?;
        self.ids(
            results::live_turn::SAME_TURN_CANDIDATE_IDS_SQL,
            rusqlite::params![
                self.scope.agent_id,
                turn,
                crate::runtime_db::migrations::timestamp(cutoff),
                32
            ],
        )
    }

    pub(in crate::runtime) fn same_turn_candidate(
        &self,
        identity: &str,
        turn: &str,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<QualifiedResultRead>> {
        resource_io::check_active()?;
        let Some(record) = self.record(identity)? else {
            return Ok(None);
        };
        // Addresses are not grants: qualify only the consumed original, then
        // preserve every same-parent, pending, cutoff and source predicate.
        Ok(results::live_turn::same_turn_candidate_is_live_tx(
            self.tx,
            &record,
            &self.scope.agent_id,
            turn,
            cutoff,
        )?
        .then_some(record))
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

    /// Trials reuse immutable previews; changed projections get a new charge.
    pub(in crate::runtime) fn pending_text(
        &self,
        anchor: &MessageEnvelope,
        records: &[impl std::borrow::Borrow<TaskResultSettlementRecord>],
    ) -> Result<Option<String>> {
        self.prepare_pending_previews(
            records
                .iter()
                .map(std::borrow::Borrow::borrow)
                .map(PendingResultView::from),
        )?;
        let cached = self.pending_previews.borrow();
        // The byte counter and final formatter each deduplicate up to one
        // identity and two Message IDs per Result. Reserve their tree slots
        // as well as the aggregate preview references before formatting.
        resource_io::charge_owned_metadata(
            u64::try_from(
                records.len()
                    * (std::mem::size_of::<&crate::context::PendingTaskResultPreview>() + 6 * 64),
            )?,
            7 * records.len(),
        )?;
        let mut previews = Vec::with_capacity(records.len());
        for (index, record) in records.iter().enumerate() {
            let record = record.borrow();
            if record.message_id == anchor.id
                || records[..index]
                    .iter()
                    .any(|prior| prior.borrow().result_identity == record.result_identity)
            {
                continue;
            }
            previews.push(&cached[&record.result_identity].preview);
        }
        let bytes =
            crate::context::pending_task_results_preview_bytes(previews.iter().copied(), anchor);
        resource_io::charge_owned_payload(u64::try_from(bytes)?)?;
        Ok(crate::context::render_pending_task_result_previews(
            previews.into_iter(),
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
