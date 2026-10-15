//! Closed original Queue/Audit operations, borrowing one actual business TX.
use super::*;
use writes::QueueWrite;

pub(crate) struct CurrentReadBusinessTransaction<'tx, 'conn> {
    proof: writes::CurrentReadWriteProof<'tx, 'conn>,
    actual: BTreeMap<CurrentReadKey, writes::CurrentReadActualWrite<'tx, 'conn>>,
}
impl<'tx, 'conn> CurrentReadBusinessTransaction<'tx, 'conn> {
    pub(in crate::runtime_db) fn narrow_original_remaining(&mut self, tx:&Transaction<'_>,remaining:(usize,usize)) -> Result<()> {
        self.proof.narrow_original_remaining(tx,remaining)
    }
    pub(in crate::runtime_db) fn input_writer_material(
        &self,
        tx: &Transaction<'_>,
    ) -> Result<ObservedCurrentReadMaterial<'_>> {
        self.proof.input_writer_material(tx)
    }
    pub(in crate::runtime_db) fn prepare_active_audit_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        state: &AgentState,
        keys: &BTreeSet<CurrentReadKey>,
        request: &CurrentReadRequest,
        ledger: &InputEvidenceBudgetLedger,
        prior: (usize, usize),
        guards: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<Self> {
        Ok(Self {
            proof: writes::prepare_active_audit_write_tx(
                tx, scope, state, keys, request, ledger, prior, guards,
            )?,
            actual: BTreeMap::new(),
        })
    }

    pub(crate) fn prepare_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        state: &AgentState,
        keys: &BTreeSet<CurrentReadKey>,
        request: &CurrentReadRequest,
        ledger: &InputEvidenceBudgetLedger,
        prior: (usize, usize),
        guards: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<Self> {
        Ok(Self {
            proof: writes::prepare_original_business_write_tx(
                tx, scope, state, keys, request, ledger, prior, guards,
            )?,
            actual: BTreeMap::new(),
        })
    }
    pub(crate) fn prepare_state_source_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        state: &AgentState,
        request: &CurrentReadRequest,
        ledger: &InputEvidenceBudgetLedger,
        prior: (usize, usize),
        guards: Vec<Arc<BudgetViewGuard>>,
        source: Arc<ResidentCurrentReadFacts>,
    ) -> Result<Self> {
        Ok(Self {
            proof: writes::prepare_original_state_source_tx(
                tx, scope, state, request, ledger, prior, guards, source,
            )?,
            actual: BTreeMap::new(),
        })
    }
    pub(crate) fn prepare_actual_unchanged_state_source_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        request: &CurrentReadRequest,
        ledger: &InputEvidenceBudgetLedger,
        source: Arc<ResidentCurrentReadFacts>,
    ) -> Result<(Self, AgentState)> {
        let (proof, state) =
            writes::prepare_actual_unchanged_state_source_tx(tx, scope, request, ledger, source)?;
        Ok((
            Self {
                proof,
                actual: BTreeMap::new(),
            },
            state,
        ))
    }
    pub(super) fn prepare_actual_original_maintenance_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        keys: &BTreeSet<CurrentReadKey>,
        request: &CurrentReadRequest,
        ledger: &InputEvidenceBudgetLedger,
        prior: (usize, usize),
        guards: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<(Self, AgentState)> {
        let (proof, state) = writes::prepare_actual_original_maintenance_tx(
            tx, scope, keys, request, ledger, prior, guards,
        )?;
        Ok((
            Self {
                proof,
                actual: BTreeMap::new(),
            },
            state,
        ))
    }
    pub(crate) fn prepare_original_maintenance_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        state: &AgentState,
        keys: &BTreeSet<CurrentReadKey>,
        request: &CurrentReadRequest,
        ledger: &InputEvidenceBudgetLedger,
        prior: (usize, usize),
        guards: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<Self> {
        Ok(Self {
            proof: writes::prepare_original_maintenance_tx(
                tx, scope, state, keys, request, ledger, prior, guards,
            )?,
            actual: BTreeMap::new(),
        })
    }
    pub(crate) fn finish_original_maintenance(
        self,
        state: &AgentState,
    ) -> Result<writes::CommittedCurrentReadMaintenance> {
        let actual = self.actual.into_values().collect::<Vec<_>>();
        writes::register_original_maintenance_tx(self.proof, &actual, state)
    }
    fn retain_actual(
        &mut self,
        mut actual: writes::CurrentReadActualWrite<'tx, 'conn>,
    ) -> Result<bool> {
        let changed = actual.changed();
        if let Some(previous) = self.actual.remove(actual.key()) {
            actual.merge_original(previous)?;
        }
        self.actual.insert(actual.key().clone(), actual);
        Ok(changed)
    }
    fn queue_tx(&mut self, tx: &Transaction<'_>, command: QueueWrite<'_>) -> Result<bool> {
        self.proof.validate_actual_tx(tx)?;
        let actual = writes::execute_queue_tx(&mut self.proof, command)?;
        self.retain_actual(actual)
    }
    pub(crate) fn upsert_queue_tx(
        &mut self,
        tx: &Transaction<'_>,
        record: &QueueEntryRecord,
    ) -> Result<bool> {
        self.queue_tx(tx, QueueWrite::Upsert(record))
    }
    pub(crate) fn observe_original_stop_queue_tx(
        &mut self,
        tx: &Transaction<'_>,
        source: &super::super::task_stop::OriginalTaskStopResultView<'_>,
    ) -> Result<()> {
        self.proof.observe_original_stop_queue_tx(tx, source)
    }
    pub(crate) fn upsert_queue_from_original_stop_tx(
        &mut self,
        tx: &Transaction<'_>,
        source: &super::super::task_stop::OriginalTaskStopResultView<'_>,
        record: &QueueEntryRecord,
    ) -> Result<bool> {
        self.proof.validate_actual_tx(tx)?;
        let message = source.message();
        source.validate_current_read_message_tx(tx, self.proof_scope(), message)?;
        ensure!(
            record.message_id == message.id
                && record.agent_id == message.agent_id
                && record.priority == message.priority
                && record.status == crate::types::QueueEntryStatus::Queued
                && record.created_at == message.created_at
                && record.updated_at == message.created_at,
            "Stop CurrentRead Queue is not the original frozen admission"
        );
        if self
            .proof
            .has_target(CurrentReadFamily::Queue, &record.message_id)
        {
            self.upsert_queue_tx(tx, record)
        } else {
            // Only this authentic frozen Stop can preserve its raw old Queue
            // responsibility. It does not create an alias or actual member cap.
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1 AND agent_id=?2)",
                params![record.message_id, record.agent_id],
                |r| r.get(0),
            )?;
            ensure!(
                exists,
                "unqualified Stop Queue cleanup cannot create a new row"
            );
            repositories::upsert_queue_entry_tx(tx, record)
        }
    }
    pub(crate) fn compare_and_set_queue_tx(
        &mut self,
        tx: &Transaction<'_>,
        expected: &QueueEntryRecord,
        record: &QueueEntryRecord,
    ) -> Result<bool> {
        self.queue_tx(tx, QueueWrite::CompareAndSet { expected, record })
    }
    pub(crate) fn claim_queue_tx(
        &mut self,
        tx: &Transaction<'_>,
        record: &QueueEntryRecord,
    ) -> Result<bool> {
        self.queue_tx(tx, QueueWrite::Claim(record))
    }
    pub(crate) fn interject_queue_tx(
        &mut self,
        tx: &Transaction<'_>,
        record: &QueueEntryRecord,
    ) -> Result<bool> {
        self.queue_tx(tx, QueueWrite::Interject(record))
    }
    pub(crate) fn append_event_tx(
        &mut self,
        tx: &Transaction<'_>,
        agent: Option<&str>,
        event: &AuditEvent,
    ) -> Result<(AuditEvent, bool)> {
        self.proof.validate_actual_tx(tx)?;
        ensure!(
            agent == Some(self.proof_scope().agent_id.as_str()),
            "CurrentRead Audit business owner differs"
        );
        let (event, actual) = writes::append_current_read_event_tx(&mut self.proof, event)?;
        let changed = self.retain_actual(actual)?;
        Ok((event, changed))
    }
    fn proof_scope(&self) -> &InputEvidenceScope {
        self.proof.scope()
    }
    pub(crate) fn observe_original_control_queue_tx(&mut self, tx: &Transaction<'_>) -> Result<()> {
        self.proof.observe_original_control_queue_tx(tx)
    }
    pub(crate) fn finish(self, state: &AgentState) -> Result<CommittedCurrentReadFacts> {
        let actual = self.actual.into_values().collect::<Vec<_>>();
        writes::register_current_read_writes_tx(self.proof, &actual, state)
    }
}
