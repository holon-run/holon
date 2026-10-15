use super::*;
/// The original business transaction owns permission; this proof freezes only
/// its finite read-maintenance targets and actual SQL view.
pub(crate) struct CurrentReadWriteProof<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    scope: InputEvidenceScope,
    nonce: uuid::Uuid,
    request: CurrentReadRequest,
    ledger: InputEvidenceBudgetLedger,
    base_revision: u64,
    before_state: String,
    before_identity: String,
    original_recovery: bool,
    binding_rebound: bool,
    state_source: Option<Arc<ResidentCurrentReadFacts>>,
    projection: CurrentReadProjection,
    before: BTreeMap<CurrentReadKey, Option<(String, String)>>,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl CurrentReadWriteProof<'_, '_> {
    pub(super) fn narrow_original_remaining(&mut self, tx:&Transaction<'_>, remaining:(usize,usize)) -> Result<()> {
        self.validate_actual_tx(tx)?;
        self.request.byte_limit=self.request.byte_limit.min(self.bytes.checked_add(remaining.0)
            .context(CurrentReadError::ResourceExhausted)?);
        self.request.record_limit=self.request.record_limit.min(self.records.checked_add(remaining.1)
            .context(CurrentReadError::ResourceExhausted)?);
        self.request.check_material(self.bytes,self.records)
    }
    pub(super) fn input_writer_material(
        &self,
        tx: &Transaction<'_>,
    ) -> Result<ObservedCurrentReadMaterial<'_>> {
        self.validate_actual_tx(tx)?;
        Ok(ObservedCurrentReadMaterial {
            scope: &self.scope, ledger: &self.ledger,
            usage: (self.bytes, self.records), guards: &self.guards,
        })
    }
}
enum CurrentReadProjection {
    Ready,
    Maintenance(Option<loader::SavedCoverage>),
    FocusedEvents(Option<loader::SavedCoverage>),
}
#[derive(Debug)]
pub(crate) struct CommittedCurrentReadMaintenance {
    pub base_revision: u64,
    pub revision: u64,
    snapshot: Option<CurrentReadSnapshot>,
    _material: Vec<Arc<BudgetViewGuard>>,
}
impl CommittedCurrentReadMaintenance {
    pub(crate) fn resident(&self) -> Option<Arc<ResidentCurrentReadFacts>> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.resident.clone())
    }
}
/// Only the actual original write/readback functions mint this non-Clone value.
pub(crate) struct CurrentReadActualWrite<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    nonce: uuid::Uuid,
    key: CurrentReadKey,
    changed: bool,
    content: String,
    projection: String,
}

fn event_birth_source_installed_tx(tx: &Transaction<'_>) -> Result<bool> {
    let (cursors, members): (bool, bool) = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='current_event_birth_cursors'),EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='current_event_native_births')",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if cursors && members {
        return Ok(true);
    }
    let required: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version>=93)",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        !required && !cursors && !members,
        CurrentReadError::NeedsPreparation
    );
    Ok(false)
}

/// This original actual-write cap is minted only by the append kernel. The
/// once finalizer must install its exact accepted member before recording birth.
pub(super) fn record_original_event_birth_tx(
    proof: &CurrentReadWriteProof<'_, '_>,
    write: &CurrentReadActualWrite<'_, '_>,
    revision: u64,
) -> Result<()> {
    if write.key.family != CurrentReadFamily::Event || !write.changed {
        return Ok(());
    }
    ensure!(
        std::ptr::eq(proof.tx, write.tx)
            && proof.nonce == write.nonce
            && proof.before.contains_key(&write.key),
        "Event birth changed original actual ACK"
    );
    if !event_birth_source_installed_tx(proof.tx)? {
        return Ok(());
    }
    let seq: u64 = proof.tx.query_row(
        "SELECT sort_seq FROM current_read_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family='event' AND record_id=?3 AND revision=?4 AND availability='present' AND content_digest=?5 AND projection_digest=?6",
        params![proof.scope.agent_id,proof.scope.identity_incarnation,write.key.id,revision,write.content,write.projection], |r| r.get(0),
    )?;
    ensure!(seq > 0, "native Event append omitted its original sequence");
    proof.tx.execute(
        "INSERT INTO current_event_birth_cursors(agent_id,identity_incarnation,high_water) VALUES(?1,?2,0) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",
        params![proof.scope.agent_id,proof.scope.identity_incarnation],
    )?;
    proof.tx.execute(
        "INSERT INTO current_event_native_births(agent_id,identity_incarnation,event_id,birth_seq) VALUES(?1,?2,?3,?4) ON CONFLICT(agent_id,identity_incarnation,event_id) DO NOTHING",
        params![proof.scope.agent_id,proof.scope.identity_incarnation,write.key.id,seq],
    )?;
    let exact: bool = proof.tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=?2 AND event_id=?3 AND birth_seq=?4)",
        params![proof.scope.agent_id,proof.scope.identity_incarnation,write.key.id,seq], |r| r.get(0),
    )?;
    ensure!(
        exact,
        "original Event birth cannot change its accepted sequence"
    );
    proof.tx.execute(
        "UPDATE current_event_birth_cursors SET high_water=MAX(high_water,?3) WHERE agent_id=?1 AND identity_incarnation=?2",
        params![proof.scope.agent_id,proof.scope.identity_incarnation,seq],
    )?;
    Ok(())
}

pub(super) fn original_native_event_cursor_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<Option<u64>> {
    if !event_birth_source_installed_tx(tx)? {
        return Ok(None);
    }
    let cursor = tx.query_row(
        "SELECT high_water FROM current_event_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation], |r| r.get(0),
    ).optional()?.unwrap_or(0);
    Ok(Some(cursor))
}
#[derive(Debug)]
pub(crate) struct CommittedCurrentReadFacts {
    pub base_revision: u64,
    pub revision: u64,
    pub snapshot: CurrentReadSnapshot,
    material_usage:(usize,usize),
}
impl CommittedCurrentReadFacts {
    pub(in crate::runtime_db) fn input_writer_material<'a>(&'a self, scope:&'a InputEvidenceScope,
        ledger:&'a InputEvidenceBudgetLedger) -> Result<ObservedCurrentReadMaterial<'a>> {
        ensure!(self.snapshot.resident.scope()==scope && self.snapshot.resident.uses_ledger(ledger),
            "committed Current material differs from original Input scope/ledger");
        let observed=ObservedCurrentReadMaterial {scope,ledger,usage:self.material_usage,guards:&self.snapshot._material};
        observed.validate_accounting(scope,ledger)?;
        Ok(observed)
    }
}
pub(crate) fn prepare_current_read_write_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<CurrentReadWriteProof<'tx, 'conn>> {
    prepare_current_read_write_inner_tx(
        tx,
        scope,
        state,
        keys,
        request,
        ledger,
        false,
        (0, 0),
        Vec::new(),
    )
}
/// Original current-owner business permission is checked by the enclosing actor.
/// This path maintains the same Active/Deleting scope; it grants no new input.
pub(super) fn prepare_original_business_write_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<CurrentReadWriteProof<'tx, 'conn>> {
    prepare_current_read_write_inner_tx(
        tx, scope, state, keys, request, ledger, true, prior, guards,
    )
}
pub(super) fn prepare_active_audit_write_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<CurrentReadWriteProof<'tx, 'conn>> {
    prepare_current_read_write_inner_tx(
        tx, scope, state, keys, request, ledger, false, prior, guards,
    )
}
fn prepare_current_read_write_inner_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    original_recovery: bool,
    prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<CurrentReadWriteProof<'tx, 'conn>> {
    request.check_material(prior.0, prior.1)?;
    let mut material = Material::new(scope, request, ledger);
    material.bytes = prior.0;
    material.records = prior.1;
    material.guards = guards;
    material.charge(
        usize::try_from(serialized_bytes(&(scope, keys, request))?)?,
        keys.len() + 8,
    )?;
    let (identity, _) = loader::scope_facts(tx, scope, state, &mut material, original_recovery)?;
    let before_state = typed_digest(state)?;
    let before_identity = typed_digest(&identity)?;
    let saved = loader::saved(tx, scope, &mut material)?;
    let binding_rebound = saved
        .as_ref()
        .is_some_and(|saved| saved.binding_invalidated || saved.records_invalidated);
    let base_revision = match saved {
        Some(saved) if !saved.binding_invalidated && !saved.records_invalidated => {
            ensure!(
                saved.state_digest == before_state && saved.identity_digest == before_identity,
                CurrentReadError::CanonicalChanged
            );
            saved.revision
        }
        saved => {
            // PreSQL preparation and its business ACK share one revision. Keep
            // the actual invalidation pending until this proof is consumed.
            let revision = saved.as_ref().map(|saved| saved.revision);
            let snapshot = loader::load(
                tx,
                scope,
                state,
                request,
                &mut material,
                revision,
                original_recovery,
            )?;
            let revision = snapshot.resident.coverage().revision;
            material.guards.extend(snapshot._material);
            drop(snapshot.resident);
            if binding_rebound {
                tx.execute("UPDATE current_read_current SET freshness='binding_invalidated' WHERE agent_id=?1 AND identity_incarnation=?2 AND revision=?3", params![scope.agent_id,scope.identity_incarnation,revision])?;
            }
            revision
        }
    };
    let mut before = BTreeMap::new();
    for key in keys {
        let observed = loader::eligible(tx, scope, key, &mut material)?;
        let (table, id, _, _) = stored::source(key.family);
        let exists: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {id}=?1)"),
            [&key.id],
            |r| r.get(0),
        )?;
        if let Some((_, availability, content, projection)) = observed {
            ensure!(
                availability == CurrentReadAvailability::Present,
                CurrentReadError::Pruned(key.clone())
            );
            let record = stored::read(tx, key, &scope.agent_id, &mut material)?
                .ok_or(CurrentReadError::CanonicalChanged)?;
            ensure!(
                record.content == content && record.projection == projection,
                CurrentReadError::CanonicalChanged
            );
            before.insert(key.clone(), Some((content, projection)));
        } else {
            let known:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM current_read_members WHERE family=?1 AND record_id=?2 UNION ALL SELECT 1 FROM current_read_fixed_members WHERE family=?1 AND record_id=?2)",params![family(key.family),key.id],|r|r.get(0))?;
            ensure!(!exists && !known, CurrentReadError::NeedsPreparation);
            before.insert(key.clone(), None);
        }
    }
    Ok(CurrentReadWriteProof {
        tx,
        scope: scope.clone(),
        nonce: uuid::Uuid::new_v4(),
        request: request.clone(),
        ledger: ledger.clone(),
        base_revision,
        before_state,
        before_identity,
        original_recovery,
        binding_rebound,
        state_source: None,
        projection: CurrentReadProjection::Ready,
        before,
        bytes: material.bytes,
        records: material.records,
        guards: material.guards,
    })
}
impl CurrentReadWriteProof<'_, '_> {
    fn readback(&mut self, key: &CurrentReadKey) -> Result<(String, String)> {
        let mut material = Material::new(&self.scope, &self.request, &self.ledger);
        material.bytes = self.bytes;
        material.records = self.records;
        let record = stored::read(self.tx, key, &self.scope.agent_id, &mut material)?
            .context("CurrentRead actual writer row missing")?;
        self.bytes = material.bytes;
        self.records = material.records;
        self.guards.extend(material.guards);
        Ok((record.content, record.projection))
    }
    fn admit_command(
        &mut self,
        value: &impl serde::Serialize,
        family: CurrentReadFamily,
        id: &str,
    ) -> Result<CurrentReadKey> {
        let key = self
            .before
            .keys()
            .find(|key| key.family == family && key.id == id)
            .context("CurrentRead writer target was not frozen before SQL")?;
        let mut material = Material::new(&self.scope, &self.request, &self.ledger);
        material.bytes = self.bytes;
        material.records = self.records;
        // Incoming Queue/Event owned values are already the business command;
        // this reservation precedes any additional original writer clone.
        let (bytes, nodes) = borrowed_current_read_material(value)?;
        material.charge_command(bytes, nodes)?;
        self.bytes = material.bytes;
        self.records = material.records;
        self.guards.extend(material.guards);
        Ok(key.clone())
    }
}
#[derive(serde::Serialize)]
pub(super) enum QueueWrite<'a> {
    Upsert(&'a QueueEntryRecord),
    CompareAndSet {
        expected: &'a QueueEntryRecord,
        record: &'a QueueEntryRecord,
    },
    Claim(&'a QueueEntryRecord),
    Interject(&'a QueueEntryRecord),
}
pub(super) fn execute_queue_tx<'tx, 'conn>(
    proof: &mut CurrentReadWriteProof<'tx, 'conn>,
    command: QueueWrite<'_>,
) -> Result<CurrentReadActualWrite<'tx, 'conn>> {
    let record = match &command {
        QueueWrite::Upsert(record) | QueueWrite::Claim(record) | QueueWrite::Interject(record) => {
            *record
        }
        QueueWrite::CompareAndSet { record, .. } => *record,
    };
    ensure!(
        record.agent_id == proof.scope.agent_id,
        "CurrentRead Queue business owner differs"
    );
    let key = proof.admit_command(&command, CurrentReadFamily::Queue, &record.message_id)?;
    let changed = match command {
        QueueWrite::Upsert(record) => repositories::upsert_queue_entry_tx(proof.tx, record)?,
        QueueWrite::CompareAndSet { expected, record } => {
            repositories::compare_and_set_queue_entry_tx(proof.tx, expected, record)?
        }
        QueueWrite::Claim(record) => repositories::try_claim_queued_message_tx(proof.tx, record)?,
        QueueWrite::Interject(record) => {
            repositories::try_interject_queued_message_tx(proof.tx, record)?
        }
    };
    let (content, projection) = proof.readback(&key)?;
    Ok(CurrentReadActualWrite {
        tx: proof.tx,
        nonce: proof.nonce,
        key,
        changed,
        content,
        projection,
    })
}
impl CurrentReadWriteProof<'_, '_> {
    pub(super) fn observe_original_control_queue_tx(&mut self, tx: &Transaction<'_>) -> Result<()> {
        self.validate_actual_tx(tx)?;
        let mut material = Material::new(&self.scope, &self.request, &self.ledger);
        material.bytes = self.bytes;
        material.records = self.records;
        // The original Control OCC query decodes this complete pending pool.
        // Raw scalar lengths precede native JSON and that original body read.
        let (count, bytes): (usize, usize) = tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(message_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(priority AS BLOB))+length(CAST(status AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+length(CAST(payload_json AS BLOB))),0) FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted','dequeued','interjected')",
            [&self.scope.agent_id], |r| Ok((r.get(0)?,r.get(1)?)),
        )?;
        material.charge(
            bytes,
            count
                .checked_add(8)
                .context("Control Queue pool count overflow")?,
        )?;
        let nodes:usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM queue_entries q,json_tree(q.payload_json) j WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted','dequeued','interjected')",
            [&self.scope.agent_id], |r| r.get(0),
        )?;
        material.charge_native(nodes)?;
        self.bytes = material.bytes;
        self.records = material.records;
        self.guards.extend(material.guards);
        Ok(())
    }
    pub(super) fn has_target(&self, family: CurrentReadFamily, id: &str) -> bool {
        self.before
            .keys()
            .any(|key| key.family == family && key.id == id)
    }
    pub(super) fn observe_original_stop_queue_tx(
        &mut self,
        tx: &Transaction<'_>,
        source: &super::super::task_stop::OriginalTaskStopResultView<'_>,
    ) -> Result<()> {
        self.validate_actual_tx(tx)?;
        source.validate_current_read_message_tx(tx, &self.scope, source.message())?;
        let id = &source.message().id;
        let mut material = Material::new(&self.scope, &self.request, &self.ledger);
        material.bytes = self.bytes;
        material.records = self.records;
        let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(message_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(priority AS BLOB))+length(CAST(status AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+length(CAST(payload_json AS BLOB))),0) FROM queue_entries WHERE message_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        material.charge(
            bytes,
            count
                .checked_add(8)
                .context("Stop Queue observation overflow")?,
        )?;
        let nodes:usize=tx.query_row("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM queue_entries q,json_tree(q.payload_json) j WHERE q.message_id=?1",[id],|r|r.get(0))?;
        material.charge_native(nodes)?;
        self.bytes = material.bytes;
        self.records = material.records;
        self.guards.extend(material.guards);
        Ok(())
    }
    pub(super) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(super) fn validate_actual_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        ensure!(
            std::ptr::eq(self.tx, tx),
            "CurrentRead business writer belongs to another transaction"
        );
        Ok(())
    }
}
impl CurrentReadActualWrite<'_, '_> {
    pub(super) fn key(&self) -> &CurrentReadKey {
        &self.key
    }
    pub(super) fn changed(&self) -> bool {
        self.changed
    }
    pub(super) fn merge_original(&mut self, before: Self) -> Result<()> {
        ensure!(
            std::ptr::eq(self.tx, before.tx)
                && self.nonce == before.nonce
                && self.key == before.key,
            "CurrentRead repeated write differs from original transaction"
        );
        self.changed |= before.changed;
        Ok(())
    }
}
pub(crate) fn upsert_current_read_queue_tx<'tx, 'conn>(
    proof: &mut CurrentReadWriteProof<'tx, 'conn>,
    record: &QueueEntryRecord,
) -> Result<CurrentReadActualWrite<'tx, 'conn>> {
    execute_queue_tx(proof, QueueWrite::Upsert(record))
}
pub(crate) fn append_current_read_event_tx<'tx, 'conn>(
    proof: &mut CurrentReadWriteProof<'tx, 'conn>,
    event: &AuditEvent,
) -> Result<(AuditEvent, CurrentReadActualWrite<'tx, 'conn>)> {
    let key = proof.admit_command(event, CurrentReadFamily::Event, &event.id)?;
    // Only an actual missing row reaches the original epoch String allocation.
    // This scalar grants no qualification or permission to change an Event.
    let exists: bool = proof.tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM audit_events WHERE audit_event_id=?1)",
        [&event.id],
        |r| r.get(0),
    )?;
    if !exists {
        // The original append reads this global scalar before constructing a new
        // typed Event. Incoming commands normally carry an empty epoch.
        let epoch_bytes: usize = proof.tx.query_row(
            "SELECT length(CAST(value AS BLOB)) FROM runtime_metadata WHERE key='event_log_epoch'",
            [],
            |r| r.get(0),
        )?;
        let mut material = Material::new(&proof.scope, &proof.request, &proof.ledger);
        material.bytes = proof.bytes;
        material.records = proof.records;
        material.charge(epoch_bytes, 1)?;
        proof.bytes = material.bytes;
        proof.records = material.records;
        proof.guards.extend(material.guards);
    }
    let (actual, changed) = super::super::evidence::append_audit_event_tx(
        proof.tx,
        Some(&proof.scope.agent_id),
        event,
    )?;
    let (content, projection) = proof.readback(&key)?;
    Ok((
        actual,
        CurrentReadActualWrite {
            tx: proof.tx,
            nonce: proof.nonce,
            key,
            changed,
            content,
            projection,
        },
    ))
}
pub(crate) fn register_current_read_writes_tx(
    mut proof: CurrentReadWriteProof<'_, '_>,
    actual: &[CurrentReadActualWrite<'_, '_>],
    final_state: &AgentState,
) -> Result<CommittedCurrentReadFacts> {
    if proof.state_source.is_some() {
        return finish_original_state_source_tx(proof, actual, final_state);
    }
    ensure!(
        matches!(&proof.projection, CurrentReadProjection::Ready),
        "maintenance proof requires its original maintenance finalizer"
    );
    let tx = proof.tx;
    let mut material = Material::new(&proof.scope, &proof.request, &proof.ledger);
    material.bytes = proof.bytes;
    material.records = proof.records;
    material.charge(
        usize::try_from(serialized_bytes(&FrozenCurrentReadKeys(&proof.before))?)?,
        actual.len() + 8,
    )?;
    let (identity, _) = loader::scope_facts(
        tx,
        &proof.scope,
        final_state,
        &mut material,
        proof.original_recovery,
    )?;
    let saved = loader::saved(tx, &proof.scope, &mut material)?
        .context("CurrentRead original coverage missing")?;
    ensure!(
        saved.revision == proof.base_revision
            && saved.state_digest == proof.before_state
            && saved.identity_digest == proof.before_identity
            && (!proof.binding_rebound || saved.binding_invalidated || saved.records_invalidated),
        CurrentReadError::RevisionMismatch
    );
    let mut changed = proof.binding_rebound
        || saved.state_digest != typed_digest(final_state)?
        || saved.identity_digest != typed_digest(&identity)?;
    let mut seen = BTreeSet::new();
    for write in actual {
        ensure!(
            std::ptr::eq(tx, write.tx)
                && write.nonce == proof.nonce
                && proof.before.contains_key(&write.key)
                && seen.insert(&write.key),
            "CurrentRead actual ACK differs from original proof"
        );
        let record = stored::read(tx, &write.key, &proof.scope.agent_id, &mut material)?
            .ok_or(CurrentReadError::CanonicalChanged)?;
        ensure!(
            record.content == write.content && record.projection == write.projection,
            CurrentReadError::CanonicalChanged
        );
        if write.changed {
            changed = true;
        } else {
            ensure!(
                proof.before[&write.key]
                    .as_ref()
                    .is_some_and(|(c, p)| c == &write.content && p == &write.projection),
                "CurrentRead noop cannot adopt an unqualified old row"
            );
        }
    }
    let revision = if changed {
        proof
            .base_revision
            .checked_add(1)
            .context("CurrentRead revision exhausted")?
    } else {
        proof.base_revision
    };
    for write in actual.iter().filter(|write| write.changed) {
        let record = stored::read(tx, &write.key, &proof.scope.agent_id, &mut material)?
            .ok_or(CurrentReadError::CanonicalChanged)?;
        tx.execute("INSERT INTO current_read_members(agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,projection_digest,status,sort_seq,sort_at,created_at) VALUES(?1,?2,?3,?4,?5,'present',?6,?7,?8,?9,?10,?11) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,availability=excluded.availability,content_digest=excluded.content_digest,projection_digest=excluded.projection_digest,status=excluded.status,sort_seq=excluded.sort_seq,sort_at=excluded.sort_at,created_at=excluded.created_at",params![proof.scope.agent_id,proof.scope.identity_incarnation,family(write.key.family),write.key.id,revision,record.content,record.projection,record.status,record.seq,record.sort_at,record.created_at])?;
        record_original_event_birth_tx(&proof, write, revision)?;
    }
    // Targets omitted from the actual ACK cannot silently change canonical facts.
    for (key, before) in &proof.before {
        if actual.iter().any(|write| &write.key == key) {
            continue;
        }
        let after = stored::read(tx, key, &proof.scope.agent_id, &mut material)?;
        ensure!(
            before.as_ref().map(|(c, p)| (c.as_str(), p.as_str()))
                == after
                    .as_ref()
                    .map(|r| (r.content.as_str(), r.projection.as_str())),
            "CurrentRead actual write set incomplete"
        );
    }
    let mut snapshot = loader::load(
        tx,
        &proof.scope,
        final_state,
        &proof.request,
        &mut material,
        Some(revision),
        proof.original_recovery,
    )?;
    snapshot._material.extend(std::mem::take(&mut proof.guards));
    proof.bytes = material.bytes;
    proof.records = material.records;
    Ok(CommittedCurrentReadFacts {
        base_revision: proof.base_revision,
        revision,
        material_usage:(proof.bytes,proof.records),
        snapshot,
    })
}

struct FrozenCurrentReadKeys<'a>(&'a BTreeMap<CurrentReadKey, Option<(String, String)>>);
impl serde::Serialize for FrozenCurrentReadKeys<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for key in self.0.keys() {
            seq.serialize_element(key)?;
        }
        seq.end()
    }
}

/// Source is an original published value, never a request for hidden hydration.
pub(super) fn prepare_original_state_source_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
    source: Arc<ResidentCurrentReadFacts>,
) -> Result<CurrentReadWriteProof<'tx, 'conn>> {
    ensure!(
        source.scope() == scope && source.uses_ledger(ledger),
        CurrentReadError::CanonicalChanged
    );
    source.validate_state(state)?;
    request.check_material(prior.0, prior.1)?;
    let mut material = Material::new(scope, request, ledger);
    material.bytes = prior.0;
    material.records = prior.1;
    material.guards = guards;
    let shared = source.material_estimate()?;
    material.share_existing(shared.0, shared.1)?;
    let (identity, _) = loader::scope_facts(tx, scope, state, &mut material, true)?;
    let saved =
        loader::saved(tx, scope, &mut material)?.ok_or(CurrentReadError::NeedsPreparation)?;
    ensure!(
        !saved.binding_invalidated && !saved.records_invalidated,
        CurrentReadError::NeedsPreparation
    );
    ensure!(
        &saved.coverage == source.coverage() && saved.identity_digest == typed_digest(&identity)?,
        CurrentReadError::RevisionMismatch
    );
    Ok(CurrentReadWriteProof {
        tx,
        scope: scope.clone(),
        nonce: uuid::Uuid::new_v4(),
        request: request.clone(),
        ledger: ledger.clone(),
        base_revision: saved.revision,
        before_state: saved.state_digest.clone(),
        before_identity: saved.identity_digest.clone(),
        original_recovery: true,
        binding_rebound: false,
        state_source: Some(source),
        projection: CurrentReadProjection::Ready,
        before: BTreeMap::new(),
        bytes: material.bytes,
        records: material.records,
        guards: material.guards,
    })
}
/// A physical-only original phase borrows one existing published source and
/// reads its actual same-incarnation State once under the same material guard.
pub(super) fn prepare_actual_unchanged_state_source_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    source: Arc<ResidentCurrentReadFacts>,
) -> Result<(CurrentReadWriteProof<'tx, 'conn>, AgentState)> {
    ensure!(
        source.scope() == scope && source.uses_ledger(ledger),
        CurrentReadError::CanonicalChanged
    );
    let mut material = Material::new(scope, request, ledger);
    let shared = source.material_estimate()?;
    material.share_existing(shared.0, shared.1)?;
    let (identity, state, _) = loader::actual_scope_facts(tx, scope, &mut material, true)?;
    source.validate_state(&state)?;
    source.validate_identity(&identity)?;
    let saved =
        loader::saved(tx, scope, &mut material)?.ok_or(CurrentReadError::NeedsPreparation)?;
    ensure!(
        !saved.binding_invalidated && !saved.records_invalidated,
        CurrentReadError::NeedsPreparation
    );
    ensure!(
        &saved.coverage == source.coverage() && saved.identity_digest == typed_digest(&identity)?,
        CurrentReadError::RevisionMismatch
    );
    Ok((
        CurrentReadWriteProof {
            tx,
            scope: scope.clone(),
            nonce: uuid::Uuid::new_v4(),
            request: request.clone(),
            ledger: ledger.clone(),
            base_revision: saved.revision,
            before_state: saved.state_digest.clone(),
            before_identity: saved.identity_digest.clone(),
            original_recovery: true,
            binding_rebound: false,
            state_source: Some(source),
            projection: CurrentReadProjection::Ready,
            before: BTreeMap::new(),
            bytes: material.bytes,
            records: material.records,
            guards: material.guards,
        },
        state,
    ))
}
fn finish_original_state_source_tx(
    mut proof: CurrentReadWriteProof<'_, '_>,
    actual: &[CurrentReadActualWrite<'_, '_>],
    final_state: &AgentState,
) -> Result<CommittedCurrentReadFacts> {
    ensure!(
        actual.is_empty() && proof.before.is_empty(),
        "State-only CurrentRead cannot ACK Queue/Event writes"
    );
    let source = proof
        .state_source
        .take()
        .context("original CurrentRead State source missing")?;
    let mut material = Material::new(&proof.scope, &proof.request, &proof.ledger);
    material.bytes = proof.bytes;
    material.records = proof.records;
    material.guards = std::mem::take(&mut proof.guards);
    let (identity, control_revision) =
        loader::scope_facts(proof.tx, &proof.scope, final_state, &mut material, true)?;
    let saved = loader::saved(proof.tx, &proof.scope, &mut material)?
        .ok_or(CurrentReadError::NeedsPreparation)?;
    ensure!(
        !saved.records_invalidated
            && saved.revision == proof.base_revision
            && saved.state_digest == proof.before_state
            && saved.identity_digest == proof.before_identity
            && &saved.coverage == source.coverage()
            && typed_digest(&identity)? == proof.before_identity,
        CurrentReadError::RevisionMismatch
    );
    let final_digest = typed_digest(final_state)?;
    let revision = if final_digest == proof.before_state {
        proof.base_revision
    } else {
        proof
            .base_revision
            .checked_add(1)
            .context("CurrentRead revision exhausted")?
    };
    // New coverage/maps are budgeted before their first clone. All bodies share
    // the exact original Arcs, so native body nodes and ledger bodies are not doubled.
    let metadata = source.metadata_estimate()?;
    material.charge(metadata.0, metadata.1)?;
    // The actual before State was validated against this source before SQL;
    // pure reconstruction only needs its unchanged digest now.
    let resident = source.rebind_state_digest(
        &proof.before_state,
        final_state,
        &identity,
        revision,
        &proof.request,
    )?;
    let changed=proof.tx.execute("UPDATE current_read_current SET revision=?3,coverage_json=?4,freshness='current' WHERE agent_id=?1 AND identity_incarnation=?2 AND revision=?5",params![proof.scope.agent_id,proof.scope.identity_incarnation,revision,serde_json::to_string(resident.coverage())?,proof.base_revision])?;
    ensure!(changed == 1, CurrentReadError::RevisionMismatch);
    Ok(CommittedCurrentReadFacts {
        base_revision: proof.base_revision,
        revision,
        material_usage:(material.bytes,material.records),
        snapshot: CurrentReadSnapshot {
            resident: Arc::new(resident),
            control_revision,
            _material: material.guards,
        },
    })
}

/// Original maintenance owns the business operation, including raw cleanup. It
/// freezes qualified targets without claiming an already-complete body domain.
pub(super) fn prepare_original_maintenance_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<CurrentReadWriteProof<'tx, 'conn>> {
    request.check_material(prior.0, prior.1)?;
    let mut material = Material::new(scope, request, ledger);
    material.bytes = prior.0;
    material.records = prior.1;
    material.guards = guards;
    material.charge(
        usize::try_from(serialized_bytes(&(scope, keys, request))?)?,
        keys.len()
            .checked_add(8)
            .context("maintenance key count overflow")?,
    )?;
    let (identity, _) = loader::scope_facts(tx, scope, state, &mut material, true)?;
    prepare_original_maintenance_after_scope_tx(
        tx, scope, state, &identity, keys, request, ledger, material,
    )
}
pub(super) fn prepare_actual_original_maintenance_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<(CurrentReadWriteProof<'tx, 'conn>, AgentState)> {
    request.check_material(prior.0, prior.1)?;
    let mut material = Material::new(scope, request, ledger);
    material.bytes = prior.0;
    material.records = prior.1;
    material.guards = guards;
    material.charge(
        usize::try_from(serialized_bytes(&(scope, keys, request))?)?,
        keys.len() + 8,
    )?;
    let (identity, state, _) = loader::actual_scope_facts(tx, scope, &mut material, true)?;
    let proof = prepare_original_maintenance_after_scope_tx(
        tx, scope, &state, &identity, keys, request, ledger, material,
    )?;
    Ok((proof, state))
}
fn prepare_original_maintenance_after_scope_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    identity: &AgentIdentityRecord,
    keys: &BTreeSet<CurrentReadKey>,
    request: &CurrentReadRequest,
    ledger: &InputEvidenceBudgetLedger,
    mut material: Material<'_>,
) -> Result<CurrentReadWriteProof<'tx, 'conn>> {
    let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_episode_continuity c WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.policy_version=1 AND ((c.mode='fresh_empty' AND c.seed_id IS NULL) OR (c.mode='legacy_adopted' AND EXISTS(SELECT 1 FROM episode_continuity_seeds s WHERE s.seed_id=c.seed_id AND s.agent_id=c.agent_id AND (s.identity_incarnation IS NULL OR s.identity_incarnation=c.identity_incarnation)))))",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    ensure!(bound, CurrentReadError::NeedsPreparation);
    let pending_import:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_episode_continuity c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND s.import_required=1 AND NOT EXISTS(SELECT 1 FROM current_read_fixed_captures f WHERE f.seed_id=s.seed_id AND f.capture_phase='first_import' AND f.sealed=1))",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    ensure!(!pending_import, CurrentReadError::NeedsPreparation);
    let before_state = typed_digest(state)?;
    let before_identity = typed_digest(&identity)?;
    let saved = loader::saved(tx, scope, &mut material)?;
    if let Some(saved) = &saved {
        ensure!(
            saved.binding_invalidated
                || saved.records_invalidated
                || (saved.state_digest == before_state && saved.identity_digest == before_identity),
            CurrentReadError::CanonicalChanged
        );
    }
    let base_revision = saved.as_ref().map_or(0, |saved| saved.revision);
    let mut before = BTreeMap::new();
    for key in keys {
        let observed = loader::eligible(tx, scope, key, &mut material)?;
        let (table, id, _, _) = stored::source(key.family);
        let exists: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {id}=?1)"),
            [&key.id],
            |r| r.get(0),
        )?;
        if let Some((_, availability, content, projection)) = observed {
            ensure!(
                availability == CurrentReadAvailability::Present,
                CurrentReadError::Pruned(key.clone())
            );
            let record = stored::read(tx, key, &scope.agent_id, &mut material)?
                .ok_or(CurrentReadError::CanonicalChanged)?;
            ensure!(
                record.content == content && record.projection == projection,
                CurrentReadError::CanonicalChanged
            );
            before.insert(key.clone(), Some((content, projection)));
        } else {
            let known:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM current_read_members WHERE family=?1 AND record_id=?2 UNION ALL SELECT 1 FROM current_read_fixed_members WHERE family=?1 AND record_id=?2)",params![family(key.family),key.id],|r|r.get(0))?;
            ensure!(!exists && !known, CurrentReadError::NeedsPreparation);
            before.insert(key.clone(), None);
        }
    }
    Ok(CurrentReadWriteProof {
        tx,
        scope: scope.clone(),
        nonce: uuid::Uuid::new_v4(),
        request: request.clone(),
        ledger: ledger.clone(),
        base_revision,
        before_state,
        before_identity,
        original_recovery: true,
        binding_rebound: false,
        state_source: None,
        projection: CurrentReadProjection::Maintenance(saved),
        before,
        bytes: material.bytes,
        records: material.records,
        guards: material.guards,
    })
}
pub(super) fn register_original_maintenance_tx(
    mut proof: CurrentReadWriteProof<'_, '_>,
    actual: &[CurrentReadActualWrite<'_, '_>],
    final_state: &AgentState,
) -> Result<CommittedCurrentReadMaintenance> {
    let CurrentReadProjection::Maintenance(before_saved) =
        std::mem::replace(&mut proof.projection, CurrentReadProjection::Ready)
    else {
        anyhow::bail!("original maintenance requires its own sealed proof")
    };
    ensure!(
        proof.state_source.is_none(),
        "State source is not a maintenance grant"
    );
    let tx = proof.tx;
    let mut material = Material::new(&proof.scope, &proof.request, &proof.ledger);
    material.bytes = proof.bytes;
    material.records = proof.records;
    material.charge(
        usize::try_from(serialized_bytes(&FrozenCurrentReadKeys(&proof.before))?)?,
        actual
            .len()
            .checked_add(8)
            .context("maintenance actual count overflow")?,
    )?;
    let (identity, _) = loader::scope_facts(tx, &proof.scope, final_state, &mut material, true)?;
    let saved = loader::saved(tx, &proof.scope, &mut material)?;
    // Business SQL only marks the normalized freshness bit. The original
    // immutable coverage/revision must remain exact until this finalizer.
    ensure!(
        before_saved.as_ref().map(|s| &s.coverage) == saved.as_ref().map(|s| &s.coverage),
        CurrentReadError::RevisionMismatch
    );
    let mut changed = before_saved
        .as_ref()
        .is_none_or(|saved| saved.binding_invalidated || saved.records_invalidated)
        || proof.before_state != typed_digest(final_state)?
        || proof.before_identity != typed_digest(&identity)?;
    let mut seen = BTreeSet::new();
    for write in actual {
        ensure!(
            std::ptr::eq(tx, write.tx)
                && write.nonce == proof.nonce
                && proof.before.contains_key(&write.key)
                && seen.insert(&write.key),
            "maintenance actual ACK differs from original proof"
        );
        let record = stored::read(tx, &write.key, &proof.scope.agent_id, &mut material)?
            .ok_or(CurrentReadError::CanonicalChanged)?;
        ensure!(
            record.content == write.content && record.projection == write.projection,
            CurrentReadError::CanonicalChanged
        );
        if write.changed {
            changed = true;
        } else {
            ensure!(
                proof.before[&write.key]
                    .as_ref()
                    .is_some_and(|(c, p)| c == &write.content && p == &write.projection),
                "maintenance noop cannot adopt raw old row"
            );
        }
    }
    let revision = if changed {
        proof
            .base_revision
            .checked_add(1)
            .context("maintenance revision exhausted")?
    } else {
        proof.base_revision
    };
    for write in actual.iter().filter(|write| write.changed) {
        let record = stored::read(tx, &write.key, &proof.scope.agent_id, &mut material)?
            .ok_or(CurrentReadError::CanonicalChanged)?;
        tx.execute("INSERT INTO current_read_members(agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,projection_digest,status,sort_seq,sort_at,created_at) VALUES(?1,?2,?3,?4,?5,'present',?6,?7,?8,?9,?10,?11) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,availability=excluded.availability,content_digest=excluded.content_digest,projection_digest=excluded.projection_digest,status=excluded.status,sort_seq=excluded.sort_seq,sort_at=excluded.sort_at,created_at=excluded.created_at",params![proof.scope.agent_id,proof.scope.identity_incarnation,family(write.key.family),write.key.id,revision,record.content,record.projection,record.status,record.seq,record.sort_at,record.created_at])?;
        record_original_event_birth_tx(&proof, write, revision)?;
    }
    for (key, before) in &proof.before {
        if actual.iter().any(|write| &write.key == key) {
            continue;
        }
        let after = stored::read(tx, key, &proof.scope.agent_id, &mut material)?;
        ensure!(
            before.as_ref().map(|(c, p)| (c.as_str(), p.as_str()))
                == after
                    .as_ref()
                    .map(|record| (record.content.as_str(), record.projection.as_str())),
            "maintenance actual write set incomplete"
        );
    }
    let projected = loader::load(
        tx,
        &proof.scope,
        final_state,
        &proof.request,
        &mut material,
        Some(revision),
        true,
    );
    match projected {
        Ok(mut snapshot) => {
            snapshot._material.extend(proof.guards);
            Ok(CommittedCurrentReadMaintenance {
                base_revision: proof.base_revision,
                revision,
                snapshot: Some(snapshot),
                _material: Vec::new(),
            })
        }
        Err(error)
            if matches!(
                error.downcast_ref::<CurrentReadError>(),
                Some(
                    CurrentReadError::UnknownNamespace(CurrentReadFamily::Queue)
                        | CurrentReadError::HistoricalUnavailable
                )
            ) =>
        {
            // Preserve only metadata about this actual ACK. It is deliberately
            // unprepared, so no Root can advertise its stale body membership.
            material.charge(2048, 64)?;
            let mut coverage =
                before_saved
                    .map(|saved| saved.coverage)
                    .unwrap_or_else(|| CurrentReadCoverage {
                        revision,
                        state_digest: String::new(),
                        identity_digest: String::new(),
                        queue_complete: false,
                        event_query: CurrentEventQueryCoverage::default(),
                        queue_counts: [
                            "queued",
                            "dequeued",
                            "processed",
                            "interjected",
                            "interrupted",
                            "aborted",
                            "dropped",
                            "quarantined",
                        ]
                        .into_iter()
                        .map(|name| (name.to_owned(), 0))
                        .collect(),
                        pending_queue: BTreeSet::new(),
                        terminal_queue: Vec::new(),
                        terminal_queue_limit: proof.request.terminal_queue_limit,
                        events: Vec::new(),
                        events_exhaustive: false,
                        event_body_limit: proof.request.event_limit,
                        event_header_limit: proof
                            .request
                            .event_limit
                            .max(proof.request.event_count_limit),
                        observations: Vec::new(),
                    });
            coverage.revision = revision;
            coverage.state_digest = typed_digest(final_state)?;
            coverage.identity_digest = typed_digest(&identity)?;
            coverage.queue_complete = false;
            material.charge(
                usize::try_from(serialized_bytes(&coverage)?)?,
                coverage
                    .observations
                    .len()
                    .checked_add(32)
                    .context("maintenance coverage count overflow")?,
            )?;
            tx.execute("INSERT INTO current_read_current(agent_id,identity_incarnation,revision,coverage_json,freshness) VALUES(?1,?2,?3,?4,'records_invalidated') ON CONFLICT(agent_id,identity_incarnation) DO UPDATE SET revision=excluded.revision,coverage_json=excluded.coverage_json,freshness='records_invalidated'",params![proof.scope.agent_id,proof.scope.identity_incarnation,revision,serde_json::to_string(&coverage)?])?;
            material.guards.extend(proof.guards);
            Ok(CommittedCurrentReadMaintenance {
                base_revision: proof.base_revision,
                revision,
                snapshot: None,
                _material: material.guards,
            })
        }
        Err(error) => Err(error),
    }
}

#[path = "focused_stop_events.rs"]
mod focused_stop_events;
pub(super) use focused_stop_events::{
    prepare_metadata_stop_events_tx, register_metadata_stop_events_tx,
};
