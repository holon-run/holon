//! Before-SQL absence observations. Birth metadata never grants body authority.
use super::*;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger,
};
use crate::runtime::resident_input_evidence::{InputQueryCoverage, OriginalInputQuerySource};

pub(in crate::runtime_db) const VERSION: i64 = 102;
/// Native material uses the original Input policy, not another domain's selector.
pub(in crate::runtime_db) fn original_material_limits(
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(usize, usize)> {
    let limits = ledger.limits();
    let bytes = limits
        .history_per_agent_bytes
        .checked_add(limits.pending_per_agent_bytes)
        .context("Input original material byte limit overflow")?;
    Ok((
        usize::try_from(bytes)?.min(MAX_METADATA_BYTES),
        usize::try_from(limits.record_limit)?.min(MAX_RECORDS),
    ))
}

pub(in crate::runtime_db) const SCHEMA: &str = r#"
CREATE TABLE input_query_birth_cursors (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(typeof(identity_incarnation)='integer' AND identity_incarnation>0),
 high_water INTEGER NOT NULL CHECK(typeof(high_water)='integer' AND high_water>=0),
 PRIMARY KEY(agent_id,identity_incarnation));
CREATE TABLE input_query_birth_members (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(typeof(identity_incarnation)='integer' AND identity_incarnation>0),
 family TEXT NOT NULL CHECK(family IN ('message','turn','tool_execution','brief','transcript','attempt','outcome','work_execution')),
 record_id TEXT NOT NULL CHECK(length(CAST(record_id AS BLOB))>0),
 birth_seq INTEGER NOT NULL CHECK(typeof(birth_seq)='integer' AND birth_seq>=0),
 PRIMARY KEY(agent_id,identity_incarnation,family,record_id),
 FOREIGN KEY(agent_id,identity_incarnation) REFERENCES input_query_birth_cursors(agent_id,identity_incarnation));
CREATE INDEX input_native_known_current ON input_evidence_current(family,record_id);
CREATE INDEX input_native_known_fixed ON input_evidence_eligibility(family,record_id);
CREATE INDEX input_native_known_recovery ON input_evidence_seed_recovery_current(family,record_id);
CREATE INDEX input_native_known_birth ON input_query_birth_members(family,record_id);
CREATE INDEX input_query_birth_frontier ON input_query_birth_members(agent_id,identity_incarnation,birth_seq,family,record_id);
CREATE UNIQUE INDEX input_query_birth_unique ON input_query_birth_members(agent_id,identity_incarnation,birth_seq) WHERE birth_seq>0;
"#;

pub(super) fn installed(tx: &Transaction<'_>) -> Result<bool> {
    let (cursors, members, migrations): (bool, bool, bool) = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='input_query_birth_cursors'),EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='input_query_birth_members'),EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if cursors && members {
        return Ok(true);
    }
    let required = migrations
        && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version>=?1)",
            [VERSION],
            |row| row.get::<_, bool>(0),
        )?;
    ensure!(
        !required && !cursors && !members,
        "installed Input birth source is incomplete"
    );
    Ok(false)
}

fn original_absent(tx: &Transaction<'_>, family: Family, id: &str) -> Result<bool> {
    let (table, column) = super::table(family);
    let address = if family == Family::Message {
        "(evidence_id=?2 OR message_id=?2)".to_owned()
    } else {
        format!("{column}=?2")
    };
    let known: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {address}) OR EXISTS(SELECT 1 FROM input_evidence_current WHERE family=?1 AND record_id=?2) OR EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE family=?1 AND record_id=?2) OR EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE family=?1 AND record_id=?2) OR EXISTS(SELECT 1 FROM input_query_birth_members WHERE family=?1 AND record_id=?2)"),
        params![family.as_str(), id], |row| row.get(0),
    )?;
    Ok(!known)
}

/// Non-Clone and concrete-TX bound; only an actual final D3 ACK consumes it.
pub(in crate::runtime_db) struct NativeInputWriteProof<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    scope: InputEvidenceScope,
    first: BTreeMap<Key, Option<NativeInputBirth>>,
    base_cursor: u64,
    limits: (usize, usize),
    usage: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
    query_source: Option<&'tx OriginalInputQuerySource>,
}

/// Exact locator scalars; scope/key already belong to the original address slot.
#[derive(Debug)]
struct NativeInputBirth {
    seq: u64,
    revision: u64,
    content_digest: [u8; 64],
}

impl<'tx, 'conn> NativeInputWriteProof<'tx, 'conn> {
    pub(in crate::runtime_db) fn prepare_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
        visit: impl FnMut(&mut dyn FnMut(Family, &str) -> Result<()>) -> Result<()>,
    ) -> Result<Option<Self>> {
        Self::prepare_with_scope_read_tx(tx, scope, ledger, limits, memory, None, visit)
    }

    pub(in crate::runtime_db) fn prepare_with_scope_read_tx(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
        scope_read: Option<&crate::runtime_db::context_inputs::ContextScopeRead<'_>>,
        mut visit: impl FnMut(&mut dyn FnMut(Family, &str) -> Result<()>) -> Result<()>,
    ) -> Result<Option<Self>> {
        if !installed(tx)? {
            return Ok(None);
        }
        let mut material = read_material::InputReadMaterial::new(scope, ledger, limits, memory)?;
        material.validate_binding(scope)?;
        let raw: usize = tx.query_row(
            "SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+256 FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id], |row| row.get(0),
        ).optional()?.ok_or(InputEvidenceSnapshotError::RetiredScope)?;
        let base_bytes = scope
            .agent_id
            .len()
            .checked_add(raw)
            .and_then(|n| n.checked_add(512))
            .context("Input birth scope overflow")?;
        let mut bytes = base_bytes;
        let mut records = 16usize;
        visit(&mut |_, id| {
            ensure!(!id.is_empty(), "Input birth original address is empty");
            bytes = address_bytes(scope, id)?
                .checked_add(bytes)
                .context("Input birth bytes overflow")?;
            records = records
                .checked_add(16)
                .context("Input birth records overflow")?;
            Ok(())
        })?;
        let bytes = bytes
            .checked_mul(8)
            .context("Input birth copies overflow")?;
        let records = records
            .checked_mul(8)
            .context("Input birth nodes overflow")?;
        let remaining = material.remaining_limits();
        ensure!(
            bytes <= remaining.0 && records <= remaining.1,
            InputEvidenceSnapshotError::ResourceExhausted("Input birth original footprint")
        );
        material.observe_native_footprint(bytes, records)?;
        let current = if let Some(read) = scope_read {
            // Borrow the canonical decode from this unchanged transaction;
            // do not materialize a second SQLite identity tree.
            read.validate_identity_scope(tx, scope)?;
            read.facts().identity.as_ref().is_some_and(|identity| {
                matches!(
                    identity.status,
                    crate::types::AgentRegistryStatus::Active
                        | crate::types::AgentRegistryStatus::Deleting
                )
            })
        } else {
            // The raw guard bounds SQLite's identity tree before its scalar probes.
            let nodes: usize = tx.query_row(
                "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities i,json_tree(i.payload_json) j WHERE i.agent_id=?1",
                [&scope.agent_id], |row| row.get(0),
            )?;
            let nodes = nodes
                .checked_add(32)
                .context("Input birth identity nodes overflow")?;
            material.observe_native_footprint(
                nodes
                    .checked_mul(256)
                    .context("Input birth identity bytes overflow")?,
                nodes,
            )?;
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1 AND status IN ('active','deleting') AND json_extract(payload_json,'$.agent_id')=?1 AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' THEN json_extract(payload_json,'$.incarnation') END=?2 AND json_extract(payload_json,'$.status')=status)",
                params![scope.agent_id, scope.identity_incarnation], |row| row.get(0),
            )?
        };
        ensure!(current, InputEvidenceSnapshotError::RetiredScope);
        tx.execute(
            "INSERT INTO input_query_birth_cursors(agent_id,identity_incarnation,high_water) VALUES(?1,?2,0) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",
            params![scope.agent_id, scope.identity_incarnation],
        )?;
        let base_cursor = cursor_tx(tx, scope)?;
        let mut copied_bytes = base_bytes;
        let mut copied_records = 16usize;
        let mut first = BTreeMap::new();
        visit(&mut |family, id| {
            copied_bytes = address_bytes(scope, id)?
                .checked_add(copied_bytes)
                .context("Input copied bytes overflow")?;
            copied_records = copied_records
                .checked_add(16)
                .context("Input copied nodes overflow")?;
            ensure!(
                !id.is_empty()
                    && copied_bytes.checked_mul(8).is_some_and(|n| n <= bytes)
                    && copied_records.checked_mul(8).is_some_and(|n| n <= records),
                InputEvidenceSnapshotError::ResourceExhausted(
                    "Input birth borrowed footprint changed"
                )
            );
            if original_absent(tx, family, id)? {
                first.insert(Key::new(family, id), None);
            }
            Ok(())
        })?;
        ensure!(
            copied_bytes.checked_mul(8) == Some(bytes)
                && copied_records.checked_mul(8) == Some(records),
            "Input birth footprint changed between borrowed passes"
        );
        let (limits, usage, guards) = material.retain();
        Ok(Some(Self {
            tx,
            scope: scope.clone(),
            first,
            base_cursor,
            limits,
            usage,
            guards,
            query_source: None,
        }))
    }

    /// Bind only an owner-frozen baseline, before the original business SQL.
    pub(in crate::runtime_db) fn bind_query_source(
        &mut self,
        source: Option<&'tx OriginalInputQuerySource>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.query_source.is_none(),
            "Input query source already bound"
        );
        if let Some(source) = source {
            source.validate_binding(&self.scope, ledger)?;
            ensure!(
                source.coverage().birth_cursor == self.base_cursor,
                "Input query source differs from original birth frontier"
            );
        }
        self.query_source = source;
        Ok(())
    }

    /// Continue the original allowance, never a fresh post-write Native budget.
    pub(in crate::runtime_db) fn with_input_phase<T>(
        &mut self,
        tx: &Transaction<'_>,
        ledger: &InputEvidenceBudgetLedger,
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
        action: impl FnOnce(&mut read_material::InputReadMaterial<'_>) -> Result<T>,
    ) -> Result<T> {
        ensure!(
            std::ptr::eq(self.tx, tx),
            "Native Input phase changed original transaction"
        );
        for guard in &self.guards {
            guard.validate_owned_material_binding(&self.scope, ledger, 0)?;
        }
        if let Some(memory) = memory.as_deref() {
            memory.validate_input_accounting(&self.scope, ledger)?;
        }
        let mut material = read_material::InputReadMaterial::resume_native_phase(
            &self.scope,
            ledger,
            self.limits,
            self.usage,
            std::mem::take(&mut self.guards),
            memory,
        )?;
        let result = action(&mut material);
        let (limits, usage, guards) = material.retain();
        self.limits = limits;
        self.usage = usage;
        self.guards = guards;
        result
    }

    pub(in crate::runtime_db) fn finish_delta_tx(
        mut self,
        tx: &Transaction<'_>,
        delta: &mut CommittedInputEvidenceDelta,
        ledger: &InputEvidenceBudgetLedger,
        mut memory: Option<
            &mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
        >,
    ) -> Result<()> {
        ensure!(
            delta.query_advance.is_none(),
            "Input delta already has an original birth advance"
        );
        // Validate the original phase even when no allocation remains. The
        // pre-admitted slots become the ACK; no second map/locator is built.
        self.with_input_phase(tx, ledger, memory.as_deref_mut(), |_| Ok(()))?;
        ensure!(
            cursor_tx(tx, &self.scope)? == self.base_cursor,
            "Input birth cursor changed within original transaction"
        );
        for record in &delta.records {
            let locator = &record.locator;
            ensure!(
                locator.scope == self.scope,
                "Input birth actual ACK crossed scope"
            );
            let exact: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 AND revision=?5 AND content_digest=?6 AND availability='present' AND qualification='committed_writer')",
                params![self.scope.agent_id, self.scope.identity_incarnation,
                    locator.key.family.as_str(), locator.key.id, locator.revision, locator.content_digest],
                |row| row.get(0),
            )?;
            ensure!(
                exact,
                "Input birth lacks exact actual D3 writer ACK: {:?} revision {}",
                locator.key,
                locator.revision
            );
            let existing: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4)",
                params![self.scope.agent_id, self.scope.identity_incarnation, locator.key.family.as_str(), locator.key.id],
                |row| row.get(0),
            )?;
            if existing {
                continue;
            }
            let seq: u64 = if self.first.contains_key(&locator.key) {
                tx.query_row(
                    "UPDATE input_query_birth_cursors SET high_water=high_water+1 WHERE agent_id=?1 AND identity_incarnation=?2 AND high_water<9223372036854775807 RETURNING high_water",
                    params![self.scope.agent_id, self.scope.identity_incarnation], |row| row.get(0),
                )?
            } else {
                0
            };
            tx.execute(
                "INSERT INTO input_query_birth_members(agent_id,identity_incarnation,family,record_id,birth_seq) VALUES(?1,?2,?3,?4,?5)",
                params![self.scope.agent_id, self.scope.identity_incarnation, locator.key.family.as_str(), locator.key.id, seq],
            )?;
            if seq > 0 {
                *self
                    .first
                    .get_mut(&locator.key)
                    .expect("original birth slot") = Some(NativeInputBirth {
                    seq,
                    revision: locator.revision,
                    content_digest: locator
                        .content_digest
                        .as_bytes()
                        .try_into()
                        .context("Input birth ACK digest is not SHA-256")?,
                });
            }
        }
        self.first.retain(|_, birth| birth.is_some());
        let born = std::mem::take(&mut self.first);
        let cursor = cursor_tx(tx, &self.scope)?;
        ensure!(
            self.base_cursor.checked_add(u64::try_from(born.len())?) == Some(cursor),
            "Input birth advance is not the complete original transaction frontier"
        );
        let publication = if let Some(source) = self.query_source {
            Some(self.with_input_phase(tx, ledger, memory, |material| {
                let nodes = source
                    .coverage()
                    .born
                    .len()
                    .checked_add(source.coverage().required_keys.len())
                    .and_then(|n| n.checked_add(source.material().len()))
                    .and_then(|n| n.checked_add(born.len()))
                    .and_then(|n| n.checked_add(16))
                    .and_then(|n| n.checked_mul(8))
                    .context("Input query publication nodes overflow")?;
                let bytes = usize::try_from(serialized_bytes(source.coverage().as_ref())?)?
                    .checked_mul(8)
                    .and_then(|n| n.checked_add(nodes.checked_mul(256)?))
                    .context("Input query publication bytes overflow")?;
                material.observe_native_footprint(bytes, nodes)?;
                let mut coverage = source.coverage().as_ref().clone();
                coverage.birth_cursor = cursor;
                if coverage.birth_after.is_some() {
                    for (key, birth) in &born {
                        let seq = birth.as_ref().expect("actual birth").seq;
                        ensure!(
                            coverage.born.insert(seq, key.clone()).is_none(),
                            "Input query publication duplicated birth"
                        );
                    }
                }
                coverage.validate()?;
                Ok(CommittedInputQueryPublication {
                    base: source.baseline().clone(),
                    coverage: Arc::new(coverage),
                    _base_material: source.material().to_vec(),
                    acquisition: source.acquisition().clone(),
                })
            })?)
        } else {
            None
        };
        delta.query_advance = Some(Arc::new(CommittedInputQueryAdvance {
            scope: self.scope,
            base_cursor: self.base_cursor,
            cursor,
            born,
            _material: self.guards,
            publication,
        }));
        Ok(())
    }
}

/// Non-Serde actual facts, constructed only by consuming the original TX proof.
#[derive(Debug)]
pub(crate) struct CommittedInputQueryAdvance {
    scope: InputEvidenceScope,
    base_cursor: u64,
    cursor: u64,
    born: BTreeMap<Key, Option<NativeInputBirth>>,
    _material: Vec<Arc<BudgetViewGuard>>,
    publication: Option<CommittedInputQueryPublication>,
}

#[derive(Debug)]
struct CommittedInputQueryPublication {
    base: Arc<InputQueryCoverage>,
    coverage: Arc<InputQueryCoverage>,
    _base_material: Vec<Arc<BudgetViewGuard>>,
    acquisition: crate::diagnostics::resource_io::ResourceReadMaterial,
}

impl CommittedInputQueryAdvance {
    pub(crate) fn install_query(
        &self,
        base: Option<&Arc<InputQueryCoverage>>,
        ledger: Option<&InputEvidenceBudgetLedger>,
    ) -> Result<
        Option<(
            Arc<InputQueryCoverage>,
            Vec<Arc<BudgetViewGuard>>,
            crate::diagnostics::resource_io::ResourceReadMaterial,
        )>,
    > {
        let Some(publication) = &self.publication else {
            return Ok(None);
        };
        ensure!(
            base.is_some_and(|base| Arc::ptr_eq(base, &publication.base)),
            "Input query installer changed original accepted baseline"
        );
        let ledger = ledger.context("Input query installer has no original ledger")?;
        for guard in &self._material {
            guard.validate_owned_material_binding(&self.scope, ledger, 0)?;
        }
        publication.acquisition.read(|| {
            let bytes = self
                ._material
                .len()
                .checked_mul(std::mem::size_of::<Arc<BudgetViewGuard>>())
                .context("Input query install guards overflow")?;
            crate::diagnostics::resource_io::charge_owned_metadata(
                u64::try_from(bytes)?,
                self._material.len(),
            )?;
            Ok(Some((
                publication.coverage.clone(),
                self._material.clone(),
                publication.acquisition.clone(),
            )))
        })
    }

    #[cfg(test)]
    pub(in crate::runtime_db) fn weak_guard_for_fixture(&self) -> std::sync::Weak<BudgetViewGuard> {
        Arc::downgrade(&self._material[0])
    }

    pub(crate) fn validate_delta(
        &self,
        scope: &InputEvidenceScope,
        records: &[Arc<QualifiedInputEvidence>],
    ) -> Result<()> {
        ensure!(
            self.scope == *scope,
            "Input birth advance belongs to another scope"
        );
        ensure!(
            self.base_cursor
                .checked_add(u64::try_from(self.born.len())?)
                == Some(self.cursor),
            "Input birth advance frontier differs"
        );
        for (key, birth) in &self.born {
            let birth = birth
                .as_ref()
                .context("Input birth advance has an empty slot")?;
            ensure!(
                birth.seq > self.base_cursor
                    && birth.seq <= self.cursor
                    && records
                        .iter()
                        .any(|record| record.locator.scope == self.scope
                            && record.locator.key == *key
                            && record.locator.revision == birth.revision
                            && record.locator.content_digest.as_bytes() == birth.content_digest),
                "Input birth advance lacks its exact actual D3 record"
            );
        }
        Ok(())
    }
}

fn address_bytes(scope: &InputEvidenceScope, id: &str) -> Result<usize> {
    id.len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(scope.agent_id.len()))
        .and_then(|n| n.checked_add(512))
        .context("Input birth address overflow")
}

fn cursor_tx(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<u64> {
    Ok(tx.query_row(
        "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id, scope.identity_incarnation], |row| row.get(0),
    )?)
}

pub(in crate::runtime_db) fn visit_execution_commands(
    commands: &[crate::domain::execution_protocol::ExecutionProtocolCommand],
    visit: &mut dyn FnMut(Family, &str) -> Result<()>,
) -> Result<()> {
    use crate::domain::execution_protocol::ExecutionProtocolCommand as C;
    for command in commands {
        match command {
            C::RegisterWorkItem(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::AdvanceWorkItemSourceRevision(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::ReconcileWorkItemSourceRevision(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::SetWorkItemReadiness(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::RecheckContextBudgetStop(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::SuspendWorkItemContinuation(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::ResumeWorkItemContinuation(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::ReconcileWorkItemContinuationYield(c) => {
                visit(Family::WorkExecution, &c.work_item_id)?
            }
            C::SetWorkItemWaiting(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::CompleteWorkItem(c) => visit(Family::WorkExecution, &c.work_item_id)?,
            C::Admit(c) => visit(Family::Attempt, &c.attempt.attempt_id)?,
            C::Settle(c) => visit(Family::Outcome, &c.outcome.outcome_id)?,
            C::Interrupt(c) => visit(Family::Outcome, &c.outcome_id)?,
            C::FinalizeInterrupted(c) => visit(Family::Attempt, &c.attempt_id)?,
            C::RecoverInterruptedTaskResultClaim(c) => visit(Family::Outcome, &c.outcome_id)?,
            C::RecoverUnadvancedTaskResultClaim(c) => visit(Family::Outcome, &c.outcome_id)?,
        }
    }
    Ok(())
}
