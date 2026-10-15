//! Original native INSERT observations. This metadata never grants Input body authority.
use super::*;
use crate::runtime::input_evidence_budget::{
    BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};

pub(in crate::runtime_db) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS input_query_birth_cursors (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(typeof(identity_incarnation)='integer' AND identity_incarnation>0),
 high_water INTEGER NOT NULL CHECK(typeof(high_water)='integer' AND high_water>=0),
 PRIMARY KEY(agent_id,identity_incarnation));
CREATE TABLE IF NOT EXISTS input_query_birth_members (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(typeof(identity_incarnation)='integer' AND identity_incarnation>0),
 family TEXT NOT NULL CHECK(family IN ('message','turn','tool_execution','brief','legacy_delivery','transcript','attempt','outcome','work_execution')),
 record_id TEXT NOT NULL CHECK(length(CAST(record_id AS BLOB))>0), birth_seq INTEGER NOT NULL CHECK(typeof(birth_seq)='integer' AND birth_seq>=0),
 PRIMARY KEY(agent_id,identity_incarnation,family,record_id),
 FOREIGN KEY(agent_id,identity_incarnation) REFERENCES input_query_birth_cursors(agent_id,identity_incarnation));
CREATE INDEX IF NOT EXISTS input_native_known_current ON input_evidence_current(family,record_id);
CREATE INDEX IF NOT EXISTS input_native_known_fixed ON input_evidence_eligibility(family,record_id);
CREATE INDEX IF NOT EXISTS input_native_known_recovery ON input_evidence_seed_recovery_current(family,record_id);
CREATE INDEX IF NOT EXISTS input_native_known_birth ON input_query_birth_members(family,record_id);
CREATE INDEX IF NOT EXISTS input_query_birth_frontier ON input_query_birth_members(agent_id,identity_incarnation,birth_seq,family,record_id);
CREATE UNIQUE INDEX IF NOT EXISTS input_query_birth_unique ON input_query_birth_members(agent_id,identity_incarnation,birth_seq) WHERE birth_seq>0;
"#;

/// Optional historical body tables may be absent at the original cut. Index
/// only actual tables; none of these statements discovers or adopts a row.
pub(in crate::runtime_db) fn initialize_indexes_tx(tx: &Transaction<'_>) -> Result<()> {
    for (table, column, index) in [
        (
            "execution_protocol_attempts",
            "attempt_id",
            "input_native_attempt_address",
        ),
        (
            "execution_protocol_outcomes",
            "outcome_id",
            "input_native_outcome_address",
        ),
        (
            "execution_protocol_work_items",
            "work_item_id",
            "input_native_work_address",
        ),
    ] {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if exists {
            tx.execute_batch(&format!(
                "CREATE INDEX IF NOT EXISTS {index} ON {table}({column})"
            ))?;
        }
    }
    Ok(())
}

pub(super) fn installed(tx: &Transaction<'_>) -> Result<bool> {
    let (cursors, members): (bool, bool) = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='input_query_birth_cursors'),EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='input_query_birth_members')",
        [], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    if cursors && members {
        return Ok(true);
    }
    let required: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version>=92)",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        !required && !cursors && !members,
        "installed Input birth source is incomplete"
    );
    Ok(false)
}
fn current_scope(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<()> {
    // Only a boolean precedes the original actor's body material gates. The
    // enclosing writer still owns State/admission and D3 qualification checks.
    let current: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1 AND status IN ('active','deleting') AND json_extract(payload_json,'$.agent_id')=?1 AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' THEN json_extract(payload_json,'$.incarnation') END=?2 AND json_extract(payload_json,'$.status')=status)",
        params![scope.agent_id,scope.identity_incarnation], |row| row.get(0),
    )?;
    ensure!(current, InputEvidenceSnapshotError::RetiredScope);
    Ok(())
}
pub(super) fn initialize(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<()> {
    tx.execute(
        "INSERT INTO input_query_birth_cursors(agent_id,identity_incarnation,high_water) VALUES(?1,?2,0) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",
        params![scope.agent_id,scope.identity_incarnation],
    )?;
    Ok(())
}
fn original_absent(tx: &Transaction<'_>, family: Family, id: &str) -> Result<bool> {
    if family == Family::LegacyDelivery {
        // This family currently has only original fixed/import producers.
        return Ok(false);
    }
    let (table, column) = super::table(family);
    let address = if family == Family::Message {
        "(evidence_id=?2 OR message_id=?2)".to_owned()
    } else {
        format!("{column}=?2")
    };
    let known: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {address}) OR EXISTS(SELECT 1 FROM input_evidence_current WHERE family=?1 AND record_id=?2) OR EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE family=?1 AND record_id=?2) OR EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE family=?1 AND record_id=?2) OR EXISTS(SELECT 1 FROM input_query_birth_members WHERE family=?1 AND record_id=?2)"),
        params![family.as_str(),id], |row| row.get(0),
    )?;
    Ok(!known)
}

/// Non-Clone/non-Serde and tied to the concrete transaction before business SQL.
/// It observes absence; only the original final D3 ACK can consume it.
pub(in crate::runtime_db) struct NativeInputWriteProof<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    scope: InputEvidenceScope,
    first: BTreeSet<Key>,
    original_tool_noops: Vec<tool_writer::OriginalToolNoopReadback<'tx, 'conn>>,
    base_cursor: u64,
    _material: Vec<Arc<BudgetViewGuard>>,
    query_source: Option<Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
    input_phase_usage: Option<(usize, usize)>,
    input_phase_limits: Option<(usize, usize)>,
}
impl<'tx, 'conn> NativeInputWriteProof<'tx, 'conn> {
    #[cfg(test)]
    pub(in crate::runtime_db) fn prepare_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        visit: impl FnMut(&mut dyn FnMut(Family, &str) -> Result<()>) -> Result<()>,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<Option<Self>> {
        Self::prepare_with_query_source_tx(tx, scope, visit, ledger, limits, None)
    }
    pub(in crate::runtime_db) fn prepare_with_query_source_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        visit: impl FnMut(&mut dyn FnMut(Family, &str) -> Result<()>) -> Result<()>,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
        query_source: Option<&Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
    ) -> Result<Option<Self>> {
        // Original selector guards are already real Host/standalone allocations.
        // Debit their cost before the scope or first native key is copied.
        let limits = match query_source {
            Some(source) => source.remaining_limits(scope, ledger, limits)?,
            None => limits,
        };
        Self::prepare_observed_tx(tx, scope, visit, limits, query_source, |bytes, _records| {
            Ok(ledger.register_owned_bytes(
                scope, InputMemoryCategory::Candidate,
                OwnedBytesEstimate { payload_bytes: 0, metadata_estimate_bytes: u64::try_from(bytes)? },
            )?)
        })
    }
    pub(in crate::runtime_db) fn prepare_using_input_material_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        visit: impl FnMut(&mut dyn FnMut(Family, &str) -> Result<()>) -> Result<()>,
        material: &mut read_material::InputReadMaterial<'_>,
        query_source: Option<&Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
    ) -> Result<Option<Self>> {
        material.validate_binding(scope)?;
        if let Some(source) = query_source {
            material.observe_query_prior(source.roles(), source.owner_epoch())?;
        }
        let limits = material.remaining_material_limits()?;
        Self::prepare_observed_tx(tx, scope, visit, limits, query_source,
            |bytes, records| material.observe_native_footprint(bytes, records))
    }
    fn prepare_observed_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        mut visit: impl FnMut(&mut dyn FnMut(Family, &str) -> Result<()>) -> Result<()>,
        limits: (usize, usize),
        query_source: Option<&Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
        mut admit: impl FnMut(usize, usize) -> Result<Arc<BudgetViewGuard>>,
    ) -> Result<Option<Self>> {
        ensure!(
            !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
            InputEvidenceSnapshotError::RetiredScope
        );
        if !installed(tx)? {
            return Ok(None);
        }
        let raw: Option<usize> = tx.query_row(
            "SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+256 FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id], |row| row.get(0),
        ).optional()?;
        let raw = raw.ok_or(InputEvidenceSnapshotError::RetiredScope)?;
        let mut bytes = scope
            .agent_id
            .len()
            .checked_add(512)
            .and_then(|n| n.checked_add(raw))
            .context("Input birth scope bytes overflow")?;
        let mut nodes = 16usize;
        visit(&mut |_, id| {
            ensure!(!id.is_empty(), "Input birth original address is empty");
            bytes = bytes
                .checked_add(id.len().checked_mul(2).context("Input birth locator ID overflow")?)
                .and_then(|n| n.checked_add(scope.agent_id.len()))
                .and_then(|n| n.checked_add(512))
                .context("Input birth selector bytes overflow")?;
            nodes = nodes
                .checked_add(16)
                .context("Input birth selector nodes overflow")?;
            Ok(())
        })?;
        bytes = bytes
            .checked_mul(8)
            .context("Input birth original copies overflow")?;
        nodes = nodes
            .checked_mul(8)
            .context("Input birth original nodes overflow")?;
        ensure!(
            bytes <= limits.0.min(MAX_METADATA_BYTES) && nodes <= limits.1.min(MAX_RECORDS),
            InputEvidenceSnapshotError::ResourceExhausted("original Input birth footprint")
        );
        let material = admit(bytes, nodes)?;
        // The raw guard is already held while SQLite traverses the original
        // identity. Admit its complete native tree before further JSON probes.
        let identity_nodes: usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities i,json_tree(i.payload_json) j WHERE i.agent_id=?1",
            [&scope.agent_id], |row| row.get(0),
        )?;
        // No identity Value/DTO is decoded here; one SQLite working tree is
        // bounded. The original actor accounts for its own identity copies.
        let identity_nodes = identity_nodes
            .checked_add(32)
            .context("Input birth identity nodes overflow")?;
        let native_bytes = identity_nodes
            .checked_mul(256)
            .context("Input birth identity native bytes overflow")?;
        ensure!(
            bytes
                .checked_add(native_bytes)
                .is_some_and(|n| n <= limits.0.min(MAX_METADATA_BYTES))
                && nodes
                    .checked_add(identity_nodes)
                    .is_some_and(|n| n <= limits.1.min(MAX_RECORDS)),
            InputEvidenceSnapshotError::ResourceExhausted("original Input birth identity")
        );
        let native = admit(native_bytes, identity_nodes)?;
        current_scope(tx, scope)?;
        initialize(tx, scope)?;
        let base_cursor = tx.query_row(
            "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope.agent_id,scope.identity_incarnation], |row| row.get::<_,u64>(0),
        )?;
        let mut first = BTreeSet::new();
        let mut copied_bytes = scope
            .agent_id
            .len()
            .checked_add(512)
            .and_then(|n| n.checked_add(raw))
            .context("Input birth copied scope overflow")?;
        let mut copied_nodes = 16usize;
        visit(&mut |family, id| {
            copied_bytes = copied_bytes
                .checked_add(id.len().checked_mul(2).context("Input birth copied locator ID overflow")?)
                .and_then(|n| n.checked_add(scope.agent_id.len()))
                .and_then(|n| n.checked_add(512))
                .context("Input birth copied key overflow")?;
            copied_nodes = copied_nodes
                .checked_add(16)
                .context("Input birth copied count overflow")?;
            ensure!(
                !id.is_empty()
                    && copied_bytes.checked_mul(8).is_some_and(|n| n <= bytes)
                    && copied_nodes.checked_mul(8).is_some_and(|n| n <= nodes),
                InputEvidenceSnapshotError::ResourceExhausted(
                    "original Input birth footprint changed"
                )
            );
            if original_absent(tx, family, id)? {
                first.insert(Key::new(family, id));
            }
            Ok(())
        })?;
        ensure!(
            copied_bytes.checked_mul(8) == Some(bytes)
                && copied_nodes.checked_mul(8) == Some(nodes),
            "Input birth original footprint changed between borrowed passes"
        );
        Ok(Some(Self {
            tx,
            scope: scope.clone(),
            first,
            original_tool_noops: Vec::new(),
            base_cursor,
            _material: vec![material, native],
            query_source: query_source.cloned(),
            input_phase_usage: None,
            input_phase_limits: None,
        }))
    }
    pub(in crate::runtime_db) fn retain_input_phase(
        mut self,
        material: read_material::InputReadMaterial<'_>,
    ) -> Result<Self> {
        material.validate_binding(&self.scope)?;
        self.input_phase_usage = Some(material.usage());
        self.input_phase_limits = Some(material.original_native_limits());
        // Move the original Vec. Each Native guard is already in this phase;
        // replacing the two borrowed proof references creates no new allocation.
        self._material = material.into_guards();
        Ok(self)
    }
    pub(in crate::runtime_db) fn retain_original_tool_noops(
        &mut self, tx: &Transaction<'_>,
        readbacks: Vec<tool_writer::OriginalToolNoopReadback<'tx, 'conn>>,
    ) -> Result<()> {
        ensure!(std::ptr::eq(self.tx, tx) && self.original_tool_noops.is_empty(),
            "original no-op Tool readbacks changed Native transaction");
        for (index, readback) in readbacks.iter().enumerate() {
            let locator = readback.validate(tx, &self.scope)?;
            ensure!(!self.first.contains(&locator.key),
                "original no-op Tool cannot consume a before-SQL absent key");
            for previous in &readbacks[..index] {
                ensure!(previous.validate(tx, &self.scope)?.key != locator.key,
                    "original no-op Tool has duplicate D3 readbacks");
            }
        }
        // Move the already guarded Vec; this carries no additional allocation.
        self.original_tool_noops = readbacks;
        Ok(())
    }
    /// Only the same original TX can continue the actual pre-SQL Input phase.
    /// Restore guards and usage on either result; no Scope charge or second
    /// registration is performed while Memory is borrowed for this kernel.
    pub(in crate::runtime_db) fn with_input_phase<T>(
        &mut self,
        tx: &Transaction<'_>,
        ledger: &InputEvidenceBudgetLedger,
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
        action: impl FnOnce(&mut read_material::InputReadMaterial<'_>) -> Result<T>,
    ) -> Result<T> {
        ensure!(std::ptr::eq(self.tx, tx), "Native Input phase changed original transaction");
        let usage = self.input_phase_usage.context("Native Input writer requires its original shared phase")?;
        let limits = self.input_phase_limits.context("Native Input writer lost original phase limits")?;
        ensure!(limits.0 <= MAX_METADATA_BYTES && limits.1 <= MAX_RECORDS
            && usage.0 <= limits.0 && usage.1 <= limits.1 && !self._material.is_empty(),
            "original Native Input phase is incomplete");
        // All checks precede the move, so a rejected foreign accounting handle
        // cannot strip the original proof's guards.
        for guard in &self._material { guard.validate_owned_material_binding(&self.scope, ledger, 0)?; }
        if let Some(memory) = memory.as_deref() { memory.validate_input_accounting(&self.scope, ledger)?; }
        let mut material = read_material::InputReadMaterial::resume_native_phase(
            &self.scope, ledger, limits, usage, std::mem::take(&mut self._material), memory)?;
        let result = action(&mut material);
        self.input_phase_usage = Some(material.usage());
        self._material = material.into_guards();
        result
    }
    #[cfg(test)]
    pub(in crate::runtime_db) fn finish_tx(
        self,
        tx: &Transaction<'_>,
        records: &[Arc<QualifiedInputEvidence>],
    ) -> Result<()> {
        self.finish_advance_tx(tx, records).map(|_| ())
    }
    /// The one original outer writer attaches this only after all actual D3
    /// readbacks and before its existing whole receipt. It grants no body.
    pub(in crate::runtime_db) fn finish_delta_tx(
        self,
        tx: &Transaction<'_>,
        delta: &mut CommittedInputEvidenceDelta,
    ) -> Result<()> {
        ensure!(delta.query_advance.is_none(), "Input delta already has an original birth advance");
        match (self.input_phase_usage, self.input_phase_limits) {
            (Some(usage), Some(limits)) => ensure!(usage.0 <= limits.0 && usage.1 <= limits.1
                && !self._material.is_empty(), "original Native Input phase lost its admitted material"),
            (None, None) => {},
            _ => anyhow::bail!("original Native Input phase is incomplete"),
        }
        delta.query_advance = Some(Arc::new(self.finish_advance_tx(tx, &delta.records)?));
        Ok(())
    }
    fn finish_advance_tx(
        self,
        tx: &Transaction<'_>,
        records: &[Arc<QualifiedInputEvidence>],
    ) -> Result<CommittedInputQueryAdvance> {
        ensure!(
            std::ptr::eq(self.tx, tx),
            "Input birth proof changed original transaction"
        );
        let before = tx.query_row(
            "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
            params![self.scope.agent_id,self.scope.identity_incarnation], |row| row.get::<_,u64>(0),
        )?;
        ensure!(before == self.base_cursor, "Input birth cursor changed within original transaction");
        let mut born = BTreeMap::new();
        // Every consumed locator is the original finalizer's actual scoped D3
        // ACK below; this observation cannot authorize a body or Ready root.
        for record in records {
            let locator = &record.locator;
            ensure!(
                locator.scope == self.scope,
                "Input birth actual ACK crossed scope"
            );
            let exact: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 AND revision=?5 AND content_digest=?6 AND availability='present' AND qualification='committed_writer')",
                params![self.scope.agent_id,self.scope.identity_incarnation,locator.key.family.as_str(),locator.key.id,locator.revision,locator.content_digest], |row| row.get(0),
            )?;
            let exact_noop = if !exact && !self.first.contains(&locator.key) {
                let mut sealed = false;
                for readback in &self.original_tool_noops {
                    if readback.validate(tx, &self.scope)? == locator { sealed = true; break; }
                }
                sealed && tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='tool_execution' AND record_id=?3 AND revision=?4 AND content_digest=?5 AND availability='present' AND qualification='legacy_prepared')",
                    params![self.scope.agent_id,self.scope.identity_incarnation,locator.key.id,locator.revision,locator.content_digest],
                    |row|row.get::<_,bool>(0))?
            } else { false };
            ensure!(exact || exact_noop, "Input birth lacks exact actual D3 writer ACK");
            let existing: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4)",
                params![self.scope.agent_id,self.scope.identity_incarnation,locator.key.family.as_str(),locator.key.id], |row| row.get(0),
            )?;
            if existing {
                continue;
            }
            let seq: u64 = if self.first.contains(&locator.key) {
                tx.query_row(
                    "UPDATE input_query_birth_cursors SET high_water=high_water+1 WHERE agent_id=?1 AND identity_incarnation=?2 AND high_water<9223372036854775807 RETURNING high_water",
                    params![self.scope.agent_id,self.scope.identity_incarnation], |row| row.get(0),
                )?
            } else {
                0
            };
            tx.execute(
                "INSERT INTO input_query_birth_members(agent_id,identity_incarnation,family,record_id,birth_seq) VALUES(?1,?2,?3,?4,?5)",
                params![self.scope.agent_id,self.scope.identity_incarnation,locator.key.family.as_str(),locator.key.id,seq],
            )?;
            if seq > 0 {
                // The before-SQL footprint reserved these key/locator copies.
                born.insert(locator.key.clone(), (seq, locator.clone()));
            }
        }
        let cursor = tx.query_row(
            "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
            params![self.scope.agent_id,self.scope.identity_incarnation], |row| row.get::<_,u64>(0),
        )?;
        ensure!(self.base_cursor.checked_add(u64::try_from(born.len())?) == Some(cursor),
            "Input birth advance is not the complete original transaction frontier");
        Ok(CommittedInputQueryAdvance {
            scope: self.scope,
            base_cursor: self.base_cursor,
            cursor,
            born,
            _material: self._material,
            _query_source: self.query_source,
        })
    }
}

/// Private non-Serde facts from the consumed concrete-TX proof. Only the
/// original actual D3 finish constructs it; an ID or rowid cannot mint it.
#[derive(Debug)]
pub(crate) struct CommittedInputQueryAdvance {
    scope: InputEvidenceScope,
    base_cursor: u64,
    cursor: u64,
    born: BTreeMap<Key, (u64, crate::runtime::resident_input_evidence::EvidenceLocator)>,
    _material: Vec<Arc<BudgetViewGuard>>,
    _query_source: Option<Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
}
impl CommittedInputQueryAdvance {
    pub(crate) fn validate_delta(
        &self,
        scope: &InputEvidenceScope,
        records: &[Arc<QualifiedInputEvidence>],
    ) -> Result<()> {
        ensure!(self.scope == *scope, "Input birth advance belongs to another scope");
        ensure!(self.base_cursor.checked_add(u64::try_from(self.born.len())?) == Some(self.cursor),
            "Input birth advance frontier differs");
        for (key, (seq, locator)) in &self.born {
            ensure!(*seq > self.base_cursor && *seq <= self.cursor
                && records.iter().any(|record| record.locator == *locator && record.locator.key == *key),
                "Input birth advance lacks its exact actual D3 record");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "input_native_birth_tests.rs"]
mod tests;
