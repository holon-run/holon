//! Admission of borrowed Work selectors before their first owned copies.
use super::*;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};

/// Observes only selector/capture metadata, not a Work body or alias grant.
/// Loader raw/native material is admitted separately by the enclosing actor.
fn work_selector_cost(
    scope: &InputEvidenceScope,
    request: &WorkCaptureRequest,
) -> Result<(usize, usize, usize)> {
    let exhausted = || WorkSnapshotError::ResourceExhausted("Work selectors");
    let count = request
        .mandatory_roots
        .len()
        .checked_add(request.latest_result_work_ids.len())
        .and_then(|n| n.checked_add(request.required_turn_ids.len()))
        .and_then(|n| n.checked_add(request.required_task_ids.len()))
        .and_then(|n| n.checked_add(request.required_trigger_message_ids.len()))
        .context(exhausted())?;
    let records = count
        .checked_add(1)
        .and_then(|n| n.checked_mul(8))
        .context(exhausted())?;
    // This counting writer serializes borrowed fields without a temporary Vec,
    // String, Value, request clone or invented accounting scope.
    let payload = usize::try_from(serialized_bytes(&(
        scope,
        request.include_live,
        request.nonopen_limit,
        &request.mandatory_roots,
        &request.latest_result_work_ids,
        &request.required_turn_ids,
        &request.required_task_ids,
        &request.required_trigger_message_ids,
    ))?)?
    .checked_mul(8)
    .context(exhausted())?;
    let metadata = count
        .checked_mul(256)
        .and_then(|n| n.checked_add(std::mem::size_of::<WorkCaptureRequest>()))
        .and_then(|n| n.checked_mul(8))
        .context(exhausted())?;
    Ok((payload, metadata, records))
}

pub(crate) fn work_selector_material_estimate(
    scope: &InputEvidenceScope,
    request: &WorkCaptureRequest,
) -> Result<(usize, usize)> {
    let (payload, metadata, records) = work_selector_cost(scope, request)?;
    Ok((
        payload
            .checked_add(metadata)
            .context("Work selectors bytes overflow")?,
        records,
    ))
}

pub(crate) fn reserve_work_selector_material(
    scope: &InputEvidenceScope,
    request: &WorkCaptureRequest,
    record_limit: usize,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Arc<BudgetViewGuard>> {
    let exhausted = || WorkSnapshotError::ResourceExhausted("Work selectors");
    let (payload, metadata, records) = work_selector_cost(scope, request)?;
    let bytes = payload.checked_add(metadata).context(exhausted())?;
    ensure!(
        records <= record_limit.min(MAX_METADATA_RECORDS)
            && bytes <= byte_limit.min(MAX_METADATA_BYTES),
        exhausted()
    );
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(payload)?,
            metadata_estimate_bytes: u64::try_from(metadata)?,
        },
    )?)
}

/// An actual registered allocation estimate, not a source/record capability.
/// Only this module can construct it, after the original ledger accepted it.
pub(in crate::runtime_db) struct ObservedWorkMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    guard: Arc<BudgetViewGuard>,
    bytes: usize,
    records: usize,
}
impl ObservedWorkMaterial<'_> {
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            scope == self.scope && self.ledger.same_ledger(ledger),
            "Work material belongs to another accounting scope/ledger"
        );
        Ok((self.bytes, self.records))
    }
}

/// Only a consumed preflight phase can release its own temporary debit.
pub(in crate::runtime_db) struct ReleasedWorkMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
}
impl ReleasedWorkMaterial<'_> {
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            scope == self.scope && self.ledger.same_ledger(ledger),
            "released Work material belongs to another accounting scope/ledger"
        );
        Ok((self.bytes, self.records))
    }
}

struct DecoderMaterialMark {
    bytes: usize,
    records: usize,
    guards: usize,
}

/// One original Work acquisition's cumulative raw/native/derived material.
/// The original Memory contract additionally debits its transaction allowance.
pub(super) struct WorkReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    byte_limit: usize,
    record_limit: usize,
    bytes: usize,
    records: usize,
    original_prior: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
    wait_pool_observed: bool,
}
impl<'a> WorkReadMaterial<'a> {
    pub(super) fn new(
        scope: &'a InputEvidenceScope,
        request: &WorkDomainLoadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<Self> {
        Self::resume(
            scope,
            request,
            ledger,
            memory,
            (0, 0),
            (
                request.byte_limit.min(MAX_METADATA_BYTES),
                request.record_limit.min(MAX_METADATA_RECORDS),
            ),
        )
    }
    /// Resume the same original Work phase without observing old allocations
    /// again. The caller keeps their original guards alive.
    pub(super) fn resume(
        scope: &'a InputEvidenceScope,
        request: &WorkDomainLoadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
        prior: (usize, usize),
        original_limits: (usize, usize),
    ) -> Result<Self> {
        let exhausted = || WorkSnapshotError::ResourceExhausted("Work original phase");
        let byte_limit = original_limits.0.min(MAX_METADATA_BYTES).min(
            prior
                .0
                .checked_add(request.byte_limit.min(MAX_METADATA_BYTES))
                .context(exhausted())?,
        );
        let record_limit = original_limits.1.min(MAX_METADATA_RECORDS).min(
            prior
                .1
                .checked_add(request.record_limit.min(MAX_METADATA_RECORDS))
                .context(exhausted())?,
        );
        ensure!(
            prior.0 <= byte_limit && prior.1 <= record_limit,
            exhausted()
        );
        let mut result = Self {
            scope,
            ledger,
            memory,
            byte_limit,
            record_limit,
            bytes: prior.0,
            records: prior.1,
            original_prior: prior,
            guards: Vec::new(),
            wait_pool_observed: false,
        };
        // Constant query/guard-vector scratch is observed before constructing
        // any dynamic statement String; selector data is accounted separately.
        let scope_bytes = usize::try_from(serialized_bytes(scope)?)?
            .checked_mul(8)
            .context("Work material scope overflow")?;
        let base = std::mem::size_of::<Self>()
            .checked_add(4096)
            .and_then(|n| n.checked_add(scope_bytes))
            .context("Work material scratch overflow")?;
        result.charge(0, base, 1)?;
        Ok(result)
    }
    /// The original family Source supplies its actual expanded costs. This
    /// debits this Work/Memory phase once, with no second copy factor.
    pub(super) fn observe_projected(&mut self, bytes: u64, records: usize) -> Result<()> {
        self.charge(usize::try_from(bytes)?, 0, records)
    }
    pub(super) fn charge(&mut self, payload: usize, metadata: usize, records: usize) -> Result<()> {
        let exhausted = || WorkSnapshotError::ResourceExhausted("Work read material");
        if payload == 0 && metadata == 0 && records == 0 {
            return Ok(());
        }
        let metadata = metadata
            .checked_add(std::mem::size_of::<BudgetViewGuard>())
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<Arc<BudgetViewGuard>>()))
            .context(exhausted())?;
        let records = records.checked_add(1).context(exhausted())?;
        let bytes = payload.checked_add(metadata).context(exhausted())?;
        let next_bytes = self.bytes.checked_add(bytes).context(exhausted())?;
        let next_records = self.records.checked_add(records).context(exhausted())?;
        ensure!(
            next_bytes <= self.byte_limit && next_records <= self.record_limit,
            exhausted()
        );
        // Include the guard-vector slot before extending that owned container.
        let guard = self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(payload)?,
                metadata_estimate_bytes: u64::try_from(metadata)?,
            },
        )?;
        let observed = ObservedWorkMaterial {
            scope: self.scope,
            ledger: self.ledger,
            guard,
            bytes,
            records,
        };
        if let Some(memory) = self.memory.as_deref_mut() {
            memory.additional_observed_work_material(&observed)?;
        }
        self.guards.push(observed.guard);
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }
    pub(super) fn raw(&mut self, bytes: usize, records: usize) -> Result<()> {
        let exhausted = || WorkSnapshotError::ResourceExhausted("Work raw material");
        self.charge(
            bytes.checked_mul(8).context(exhausted())?,
            records.checked_mul(8 * 256).context(exhausted())?,
            records.checked_mul(8).context(exhausted())?,
        )
    }
    pub(super) fn native(&mut self, nodes: usize) -> Result<()> {
        let exhausted = || WorkSnapshotError::ResourceExhausted("Work native material");
        self.charge(
            0,
            nodes.checked_mul(8 * 256).context(exhausted())?,
            nodes.checked_mul(8).context(exhausted())?,
        )
    }

    fn decoder_mark(&self) -> DecoderMaterialMark {
        DecoderMaterialMark {
            bytes: self.bytes,
            records: self.records,
            guards: self.guards.len(),
        }
    }

    /// The exact decoder has returned and dropped its JSON/projection scratch.
    /// Its typed result has a separate guard before either guard is released.
    fn retire_decoder_scratch(
        &mut self,
        mark: DecoderMaterialMark,
        retained_start: Option<(usize, usize)>,
    ) -> Result<()> {
        // These four decoders add only raw, native and retained-result guards.
        // The phase's constant scratch covers the extra guard-vector capacity.
        ensure!(
            self.guards.len() <= mark.guards + 3,
            "Work decoder material escaped"
        );
        ensure!(
            self.guards[mark.guards..]
                .iter()
                .all(|guard| Arc::strong_count(guard) == 1),
            "Work decoder guard has another live holder"
        );
        let retained = retained_start
            .map(|(bytes, records)| -> Result<_> {
                Ok((
                    self.bytes
                        .checked_sub(bytes)
                        .context("Work retained byte underflow")?,
                    self.records
                        .checked_sub(records)
                        .context("Work retained count underflow")?,
                ))
            })
            .transpose()?
            .unwrap_or((0, 0));
        let released = ReleasedWorkMaterial {
            scope: self.scope,
            ledger: self.ledger,
            bytes: self.bytes.checked_sub(mark.bytes).and_then(|n| n.checked_sub(retained.0))
                .context("Work decoder byte underflow")?,
            records: self.records.checked_sub(mark.records).and_then(|n| n.checked_sub(retained.1))
                .context("Work decoder count underflow")?,
        };
        let guard = if retained_start.is_some() {
            Some(
                self.guards
                    .pop()
                    .context("Work typed result guard missing")?,
            )
        } else {
            None
        };
        drop(self.guards.drain(mark.guards..));
        if let Some(guard) = guard { self.guards.push(guard); }
        if let Some(memory) = self.memory.as_deref_mut() {
            memory.release_work_transient_material(released)?;
        }
        self.bytes = mark.bytes.checked_add(retained.0).context("Work retained bytes overflow")?;
        self.records = mark.records.checked_add(retained.1).context("Work retained records overflow")?;
        Ok(())
    }
    pub(super) fn capture_resident(
        &mut self,
        resident: &ResidentWorkDomain,
        request: &WorkCaptureRequest,
    ) -> Result<crate::runtime::resident_work::CapturedWork> {
        ensure!(
            resident.scope() == self.scope,
            "Work capture material scope differs"
        );
        let ledger = self.ledger.clone();
        let mut observer = |guard: &Arc<BudgetViewGuard>, bytes: usize, records: usize| {
            self.resident_observed(guard, bytes, records)
        };
        resident.capture_with_material(request, &ledger, &mut observer)
    }
    fn resident_observed(
        &mut self,
        guard: &Arc<BudgetViewGuard>,
        bytes: usize,
        records: usize,
    ) -> Result<()> {
        ensure!(
            guard.matches_resource_ledger(self.ledger)
                && guard.owned_material_bytes()? >= u64::try_from(bytes)?,
            "resident Work allocation is not observed by the actual phase ledger"
        );
        let next_bytes = self
            .bytes
            .checked_add(bytes)
            .context("Work selected material overflow")?;
        let next_records = self
            .records
            .checked_add(records)
            .context("Work selected count overflow")?;
        #[cfg(test)]
        if next_bytes > self.byte_limit || next_records > self.record_limit {
            eprintln!(
                "Work selected allowance: prior=({},{}) delta=({},{}) next=({},{}) limits=({},{}) guards={} guard_owned={:?}",
                self.bytes, self.records, bytes, records, next_bytes, next_records,
                self.byte_limit, self.record_limit, self.guards.len(),
                guard.owned_material_bytes(),
            );
        }
        ensure!(
            next_bytes <= self.byte_limit && next_records <= self.record_limit,
            WorkSnapshotError::ResourceExhausted("Work selected remaining allowance")
        );
        // capture_resident checked the exact resident scope before the private
        // callback. A Host address view carries allocation accounting only.
        let observed = ObservedWorkMaterial {
            scope: self.scope,
            ledger: self.ledger,
            guard: guard.clone(),
            bytes,
            records,
        };
        if let Some(memory) = self.memory.as_deref_mut() {
            memory.additional_observed_work_material(&observed)?;
        }
        if !self.guards.iter().any(|old| Arc::ptr_eq(old, guard)) {
            self.guards.push(guard.clone());
        }
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }
    pub(super) fn owning_roots_copy(&mut self, resident: &ResidentWorkDomain) -> Result<()> {
        self.coverage_copy(resident.coverage(), 2)?;
        // Only actual reference strings, not whole Work bodies or raw history.
        let mut bytes = 2048usize;
        let mut count = 1usize;
        for record in resident.borrowed_records() {
            let fields = match &record.record {
                CanonicalWorkRecord::Work(work) => [
                    work.result_brief_id.as_ref(),
                    work.completion_intent
                        .as_ref()
                        .and_then(|v| v.result_brief_id.as_ref()),
                    work.completion_intent
                        .as_ref()
                        .and_then(|v| v.source_message_id.as_ref()),
                    work.completion_intent
                        .as_ref()
                        .and_then(|v| v.source_turn_id.as_ref()),
                ],
                CanonicalWorkRecord::Wait(wait) => {
                    [wait.trigger_message_id.as_ref(), None, None, None]
                }
                CanonicalWorkRecord::Frame(_) => [None, None, None, None],
            };
            for id in fields.into_iter().flatten() {
                bytes = bytes
                    .checked_add(id.len())
                    .and_then(|n| n.checked_add(256))
                    .context("Work owning reference overflow")?;
                count = count
                    .checked_add(1)
                    .context("Work owning reference count overflow")?;
            }
        }
        self.charge(
            0,
            bytes
                .checked_mul(4)
                .context("Work owning copies overflow")?,
            count.checked_mul(4).context("Work owning nodes overflow")?,
        )
    }
    pub(super) fn ledger(&self) -> &InputEvidenceBudgetLedger {
        self.ledger
    }
    pub(super) fn scope(&self) -> &InputEvidenceScope {
        self.scope
    }
    pub(super) fn usage(&self) -> (usize, usize) {
        (self.bytes, self.records)
    }
    #[cfg(test)]
    pub(super) fn report_phase_usage(&self, stage: &str, prior: (usize, usize)) {
        if self.records > self.record_limit / 2 {
            eprintln!(
                "Work phase {stage}: prior={prior:?} used=({},{}) limits=({},{}) guards={}",
                self.bytes, self.records, self.byte_limit, self.record_limit, self.guards.len(),
            );
        }
    }
    pub(super) fn limits(&self) -> (usize, usize) {
        (self.byte_limit, self.record_limit)
    }
    pub(super) fn into_guards(self) -> Vec<Arc<BudgetViewGuard>> {
        self.guards
    }

    /// The caller has dropped the complete preflight view. Keep the original
    /// proof's material debited; this phase owns only the additional guards.
    pub(super) fn finish_transient(self) -> Result<()> {
        let released = ReleasedWorkMaterial {
            scope: self.scope,
            ledger: self.ledger,
            bytes: self.bytes.checked_sub(self.original_prior.0)
                .context("Work transient byte debit underflow")?,
            records: self.records.checked_sub(self.original_prior.1)
                .context("Work transient record debit underflow")?,
        };
        drop(self.guards);
        if let Some(memory) = self.memory {
            memory.release_work_transient_material(released)?;
        }
        Ok(())
    }

    pub(super) fn selectors(&mut self, capture: &WorkCaptureRequest) -> Result<()> {
        let count = capture
            .mandatory_roots
            .len()
            .checked_add(capture.latest_result_work_ids.len())
            .and_then(|n| n.checked_add(capture.required_turn_ids.len()))
            .and_then(|n| n.checked_add(capture.required_task_ids.len()))
            .and_then(|n| n.checked_add(capture.required_trigger_message_ids.len()))
            .context("Work selector count overflow")?;
        let bytes = usize::try_from(serialized_bytes(&(
            &capture.mandatory_roots,
            &capture.latest_result_work_ids,
            &capture.required_turn_ids,
            &capture.required_task_ids,
            &capture.required_trigger_message_ids,
        ))?)?;
        self.raw(
            bytes,
            count
                .checked_add(1)
                .context("Work selector count overflow")?,
        )
    }
    pub(super) fn keys<'b>(
        &mut self,
        keys: impl IntoIterator<Item = &'b WorkKey>,
        copies: usize,
    ) -> Result<()> {
        let mut bytes = 0usize;
        let mut count = 0usize;
        for key in keys {
            bytes = bytes
                .checked_add(key.id.len())
                .context("Work key bytes overflow")?;
            count = count.checked_add(1).context("Work key count overflow")?;
        }
        self.charge(
            bytes
                .checked_mul(copies)
                .context("Work key copies overflow")?,
            count
                .checked_mul(copies)
                .and_then(|n| n.checked_mul(256))
                .context("Work key metadata overflow")?,
            count
                .checked_mul(copies)
                .context("Work key records overflow")?,
        )
    }
    pub(super) fn coverage_copy(&mut self, coverage: &WorkCoverage, copies: usize) -> Result<()> {
        let bytes = usize::try_from(serialized_bytes(coverage)?)?;
        // Every collection entry and relation member is an independent owned
        // node. No serialization Vec or coverage clone precedes this estimate.
        let mut count = coverage
            .observations
            .len()
            .checked_add(coverage.live_work_ids.len())
            .and_then(|n| n.checked_add(coverage.unresolved_wait_ids.len()))
            .and_then(|n| n.checked_add(coverage.active_frame_ids.len()))
            .and_then(|n| n.checked_add(coverage.mandatory_roots.len()))
            .and_then(|n| n.checked_add(coverage.nonopen.keys.len()))
            .and_then(|n| n.checked_add(coverage.latest_results.len()))
            .context("Work coverage node overflow")?;
        count = count
            .checked_add(coverage.legacy_delivery_heads.len())
            .context("Work Delivery head map overflow")?;
        for heads in coverage.legacy_delivery_heads.values() {
            count = count
                .checked_add(heads.metadata_records()?)
                .context("Work Delivery head nodes overflow")?;
        }
        for relations in [
            &coverage.turn_wait_keys,
            &coverage.task_wait_keys,
            &coverage.trigger_wait_keys,
        ] {
            count = count
                .checked_add(relations.len())
                .context("Work relation node overflow")?;
            for ids in relations.values() {
                count = count
                    .checked_add(ids.len())
                    .context("Work relation member overflow")?;
            }
        }
        self.charge(
            bytes
                .checked_mul(copies)
                .context("Work coverage copies overflow")?,
            count
                .checked_add(1)
                .and_then(|n| n.checked_mul(copies))
                .and_then(|n| n.checked_mul(256))
                .context("Work coverage metadata overflow")?,
            count
                .checked_add(1)
                .and_then(|n| n.checked_mul(copies))
                .context("Work coverage records overflow")?,
        )
    }
    pub(super) fn eligible_tx(
        &mut self,
        tx: &Transaction<'_>,
        seed: Option<&str>,
        key: &WorkKey,
    ) -> Result<Option<String>> {
        let Some(seed) = seed else { return Ok(None) };
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(n),0) FROM (SELECT length(CAST(content_digest AS BLOB)) n FROM (SELECT content_digest FROM work_domain_eligibility WHERE seed_id=?1 AND family=?2 AND record_id=?3 ORDER BY CASE capture_phase WHEN 'first_import' THEN 0 ELSE 1 END LIMIT 1) UNION ALL SELECT length(CAST(original_digest AS BLOB))+length(CAST(content_digest AS BLOB)) n FROM work_domain_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3 AND family='wait')",
            params![seed,family_name(key.family),key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        qualification::eligible_digest(tx, Some(seed), key)
    }
    pub(super) fn validate_member_tx(
        &mut self,
        tx: &Transaction<'_>,
        seed: Option<&str>,
        key: &WorkKey,
        stored: &StoredRecord,
    ) -> Result<u64> {
        // Reserve both original branches before the unchanged validator reads
        // their Strings. Neither observation creates a membership grant.
        if self.alias_tx(tx, key)?.is_none() {
            let _ = self.eligible_tx(tx, seed, key)?;
        }
        qualification::validate_member(tx, self.scope, seed, key, stored)
    }
    pub(super) fn ids_query(
        &mut self,
        tx: &Transaction<'_>,
        sql: &str,
        parameters: impl rusqlite::Params,
    ) -> Result<()> {
        let (count, bytes): (usize, usize) = tx.query_row(
            &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(id AS BLOB))),0) FROM ({sql})"),
            parameters,
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        self.raw(bytes, count)
    }

    /// The original TriggerWait reducer materializes this complete owner pool.
    /// Raw/normalized lengths precede native traversal and both typed decodes.
    pub(super) fn trigger_wait_pool_tx(&mut self, tx: &Transaction<'_>) -> Result<()> {
        const POOL: &str =
            "FROM wait_conditions WHERE agent_id=?1 AND status IN ('active','triggered')";
        let (count, bytes): (usize, usize) = tx.query_row(
            &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+COALESCE(length(CAST(wait_condition_id AS BLOB)),0)+COALESCE(length(CAST(agent_id AS BLOB)),0)+COALESCE(length(CAST(work_item_id AS BLOB)),0)+COALESCE(length(CAST(status AS BLOB)),0)+COALESCE(length(CAST(kind AS BLOB)),0)+COALESCE(length(CAST(source AS BLOB)),0)+COALESCE(length(CAST(subject_ref AS BLOB)),0)+COALESCE(length(CAST(waiting_for AS BLOB)),0)+COALESCE(length(CAST(created_at AS BLOB)),0)+COALESCE(length(CAST(updated_at AS BLOB)),0)+COALESCE(length(CAST(expires_at AS BLOB)),0)+COALESCE(length(CAST(resolved_at AS BLOB)),0)+COALESCE(length(CAST(cancelled_at AS BLOB)),0)+COALESCE(length(CAST(last_turn_id AS BLOB)),0)+COALESCE(length(CAST(trigger_message_id AS BLOB)),0)+COALESCE(length(CAST(triggered_at AS BLOB)),0)+COALESCE(length(CAST(wake_sources_json AS BLOB)),0)+COALESCE(length(CAST(continuation_json AS BLOB)),0)),0) {POOL}"),
            [&self.scope.agent_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        self.raw(bytes, count)?;
        let nodes: usize = tx.query_row(
            "SELECT COALESCE(SUM((SELECT COUNT(*) FROM json_tree(w.payload_json))+(SELECT COUNT(*) FROM json_tree(w.wake_sources_json))+(SELECT COUNT(*) FROM json_tree(w.continuation_json))+19),0) FROM wait_conditions w WHERE w.agent_id=?1 AND w.status IN ('active','triggered')",
            [&self.scope.agent_id], |row| row.get(0))?;
        self.native(nodes)
    }

    pub(super) fn wait_pool_tx(&mut self, tx: &Transaction<'_>) -> Result<()> {
        if self.wait_pool_observed {
            return Ok(());
        }
        // SQLite evaluates the original relation predicate one owner row at a
        // time. Admit maximum per-row parsing scratch, not every historical
        // body as if all of them were simultaneously materialized in Rust.
        let bytes:usize=tx.query_row(
            "SELECT COALESCE(MAX(length(CAST(payload_json AS BLOB))+length(CAST(wake_sources_json AS BLOB))),0) FROM wait_conditions WHERE agent_id=?1",
            [&self.scope.agent_id],|r|r.get(0))?;
        self.raw(bytes, usize::from(bytes > 0))?;
        let nodes:usize=tx.query_row(
            "SELECT COALESCE(MAX((SELECT COUNT(*) FROM json_tree(w.payload_json))+(SELECT COUNT(*) FROM json_tree(w.wake_sources_json))),0) FROM wait_conditions w WHERE w.agent_id=?1",
            [&self.scope.agent_id],|r|r.get(0))?;
        self.native(nodes)?;
        self.wait_pool_observed = true;
        Ok(())
    }
    pub(super) fn edge_query(&mut self, tx: &Transaction<'_>, kind: &str, key: &str) -> Result<()> {
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(wait_id AS BLOB))+length(CAST(availability AS BLOB))),0) FROM (SELECT wait_id,availability FROM work_domain_wait_edges WHERE agent_id=?1 AND identity_incarnation=?2 AND edge_kind=?3 AND source_id=?4 ORDER BY wait_id LIMIT ?5)",
            params![self.scope.agent_id,self.scope.identity_incarnation,kind,key,i64::try_from(MAX_METADATA_RECORDS+1)?],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)
    }
    pub(super) fn input_ids<'b>(&mut self, ids: impl IntoIterator<Item = &'b str>) -> Result<()> {
        let (bytes, count) = ids
            .into_iter()
            .try_fold((0usize, 0usize), |(bytes, count), id| {
                Ok::<_, anyhow::Error>((
                    bytes
                        .checked_add(id.len())
                        .context("Work input selector bytes overflow")?,
                    count
                        .checked_add(1)
                        .context("Work input selector count overflow")?,
                ))
            })?;
        self.raw(bytes, count)
    }
    /// The head predicate parses qualified Brief rows sequentially in SQLite;
    /// only the exact winner is later hydrated as a D3 owning closure.
    pub(super) fn completion_result_pool_tx(
        &mut self,
        tx: &Transaction<'_>,
        work: &str,
    ) -> Result<()> {
        const POOL:&str="FROM briefs b WHERE b.agent_id=?1 AND b.work_item_id=?3 AND b.kind='result' AND (EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.family='brief' AND a.record_id=b.evidence_id AND a.agent_id=?1 AND a.identity_incarnation=?2 AND a.availability='present') OR (NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.family='brief' AND a.record_id=b.evidence_id AND a.agent_id=?1 AND a.identity_incarnation=?2) AND EXISTS(SELECT 1 FROM agent_episode_continuity c JOIN input_evidence_eligibility e USING(seed_id) WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND e.family='brief' AND e.record_id=b.evidence_id)))";
        let bytes:usize=tx.query_row(&format!("SELECT COALESCE(MAX(length(CAST(b.payload_json AS BLOB))+length(CAST(b.evidence_id AS BLOB))+length(CAST(b.created_at AS BLOB))+COALESCE(length(CAST(b.work_item_id AS BLOB)),0)+512),0) {POOL}"),
            params![self.scope.agent_id,self.scope.identity_incarnation,work],|r|r.get(0))?;
        self.raw(bytes, usize::from(bytes > 0))?;
        let nodes: usize = tx.query_row(
            &format!(
                "SELECT COALESCE(MAX((SELECT COUNT(*) FROM json_tree(b.payload_json))),0) {POOL}"
            ),
            params![self.scope.agent_id, self.scope.identity_incarnation, work],
            |r| r.get(0),
        )?;
        self.native(nodes)
    }
    pub(super) fn input_key_tx(
        &mut self,
        tx: &Transaction<'_>,
        key: &InputEvidenceKey,
    ) -> Result<()> {
        let (table, id) = match key.family {
            InputEvidenceFamily::Brief => ("briefs", "evidence_id"),
            InputEvidenceFamily::Transcript => ("transcript_entries", "evidence_id"),
            InputEvidenceFamily::Message => ("messages", "evidence_id"),
            _ => anyhow::bail!("invalid Work completion material family"),
        };
        let owned: Option<bool> = tx
            .query_row(
                &format!("SELECT agent_id=?2 FROM {table} WHERE {id}=?1"),
                params![key.id, self.scope.agent_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(owned) = owned {
            ensure!(owned, "completion source belongs to another agent");
        }
        let related = match key.family {
            InputEvidenceFamily::Transcript => "+COALESCE(length(CAST(turn_id AS BLOB)),0)",
            // D3 acquires this normalized revision through CAST(... AS TEXT).
            // INTEGER affinity alone does not bound a corrupted TEXT value.
            InputEvidenceFamily::Brief => "+COALESCE(length(CAST(created_event_seq AS BLOB)),0)",
            _ => "",
        };
        let (count,bytes):(usize,usize)=tx.query_row(
            &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(evidence_id AS BLOB))+COALESCE(length(CAST(content_hash AS BLOB)),0)+length(CAST(created_at AS BLOB)){related}+512),0) FROM {table} WHERE {id}=?1"),
            [&key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        // Alias and fixed membership can each carry an arbitrarily damaged
        // fingerprint/proof String. Observe full raw material before JSON.
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(n),0) FROM (SELECT length(CAST(fingerprint_json AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+length(CAST(availability AS BLOB)) n FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 UNION ALL SELECT length(CAST(e.fingerprint_json AS BLOB))+length(CAST(e.capture_phase AS BLOB)) FROM input_evidence_seed_membership e JOIN agent_episode_continuity c ON c.seed_id=e.seed_id WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.policy_version=1 AND e.family=?3 AND e.record_id=?4 AND NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=?4) UNION ALL SELECT length(CAST(r.fingerprint_json AS BLOB))+length(CAST(r.origin_kind AS BLOB))+COALESCE(length(CAST(r.origin_attempt_id AS BLOB)),0)+COALESCE(length(CAST(r.command_kind AS BLOB)),0)+COALESCE(length(CAST(r.command_identity AS BLOB)),0)+COALESCE(length(CAST(r.command_hash AS BLOB)),0)+COALESCE(length(CAST(r.maintenance_proof_json AS BLOB)),0)+length(CAST(s.agent_id AS BLOB))+length(CAST(s.boundary_id AS BLOB))+length(CAST(s.source_kind AS BLOB)) FROM input_evidence_seed_recovery_current r JOIN agent_episode_continuity c ON c.seed_id=r.seed_id JOIN episode_continuity_seeds s ON s.seed_id=r.seed_id WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.policy_version=1 AND r.family=?3 AND r.record_id=?4 AND NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=?4))",
            params![self.scope.agent_id,self.scope.identity_incarnation,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        if key.family == InputEvidenceFamily::Message {
            let (count,bytes):(usize,usize)=tx.query_row(
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(turn_id AS BLOB))),0) FROM conversation_input_assignments WHERE agent_id=?1 AND message_id=?2",
                params![self.scope.agent_id,key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            self.raw(bytes, count)?;
        }
        let nodes: usize = tx.query_row(
            &format!("SELECT COUNT(*) FROM {table} p,json_tree(p.payload_json) WHERE p.{id}=?1"),
            [&key.id],
            |r| r.get(0),
        )?;
        self.native(nodes)?;
        let nodes:usize=tx.query_row(
            "SELECT COUNT(*) FROM (SELECT fingerprint_json j FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 UNION ALL SELECT e.fingerprint_json FROM input_evidence_seed_membership e JOIN agent_episode_continuity c ON c.seed_id=e.seed_id WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.policy_version=1 AND e.family=?3 AND e.record_id=?4 AND NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=?4) UNION ALL SELECT r.maintenance_proof_json FROM input_evidence_seed_recovery_current r JOIN agent_episode_continuity c ON c.seed_id=r.seed_id WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.policy_version=1 AND r.family=?3 AND r.record_id=?4 AND NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=?4)) p,json_tree(p.j)",
            params![self.scope.agent_id,self.scope.identity_incarnation,key.family.as_str(),key.id],|r|r.get(0))?;
        self.native(nodes)?;
        Ok(())
    }

    pub(super) fn active_frame_keys_tx(
        &mut self,
        tx: &Transaction<'_>,
        keys: &mut BTreeSet<WorkKey>,
    ) -> Result<()> {
        let sql="SELECT continuation_id AS id FROM work_item_continuations WHERE agent_id=?1 AND state='active' ORDER BY continuation_id LIMIT 65537";
        let (count, bytes): (usize, usize) = tx.query_row(
            &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(id AS BLOB))),0) FROM ({sql})"),
            [&self.scope.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        self.raw(bytes, count)?;
        let mut query = tx.prepare(sql)?;
        for id in query.query_map([&self.scope.agent_id], |r| r.get::<_, String>(0))? {
            keys.insert(WorkKey::new(WorkFamily::Frame, id?));
        }
        Ok(())
    }
    pub(super) fn binding_tx(&mut self, tx: &Transaction<'_>) -> Result<Option<String>> {
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(mode AS BLOB))+COALESCE(length(CAST(seed_id AS BLOB)),0)),0) FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1",
            params![self.scope.agent_id,self.scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        binding(tx, self.scope)
    }
    pub(super) fn coverage_tx(&mut self, tx: &Transaction<'_>) -> Result<Option<WorkCoverage>> {
        let mark = self.decoder_mark();
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(coverage_json AS BLOB))),0) FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![self.scope.agent_id,self.scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        let nodes:usize=tx.query_row(
            "SELECT COUNT(*) FROM work_domain_current c,json_tree(c.coverage_json) WHERE c.agent_id=?1 AND c.identity_incarnation=?2",
            params![self.scope.agent_id,self.scope.identity_incarnation],|r|r.get(0))?;
        self.native(nodes)?;
        let coverage = loader::current_coverage(tx, self.scope)?;
        let retained_start = coverage.as_ref().map(|coverage| -> Result<_> {
            let start = self.usage();
            self.coverage_copy(coverage, 1)?;
            Ok(start)
        }).transpose()?;
        self.retire_decoder_scratch(mark, retained_start)?;
        Ok(coverage)
    }
    pub(super) fn alias_tx(
        &mut self,
        tx: &Transaction<'_>,
        key: &WorkKey,
    ) -> Result<Option<Alias>> {
        let mark = self.decoder_mark();
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(record_id AS BLOB))+length(CAST(family AS BLOB))+length(CAST(availability AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(fingerprint_json AS BLOB)),0)),0) FROM work_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
            params![self.scope.agent_id,self.scope.identity_incarnation,family_name(key.family),key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        let nodes:usize=tx.query_row(
            "SELECT COUNT(*) FROM work_domain_records c,json_tree(c.fingerprint_json) WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.family=?3 AND c.record_id=?4",
            params![self.scope.agent_id,self.scope.identity_incarnation,family_name(key.family),key.id],|r|r.get(0))?;
        self.native(nodes)?;
        let value = alias(tx, self.scope, key)?;
        let retained_start = value.as_ref().map(|value| -> Result<_> {
            let start = self.usage();
            let bytes = value.digest.as_ref().map_or(0, String::len)
                .checked_add(value.fingerprint.as_ref().map_or(0, |f| f.projection.len()))
                .and_then(|n| n.checked_mul(2))
                .and_then(|n| n.checked_add(std::mem::size_of::<Alias>() + 512))
                .context("Work retained alias overflow")?;
            self.charge(0, bytes, 4)?;
            Ok(start)
        }).transpose()?;
        self.retire_decoder_scratch(mark, retained_start)?;
        Ok(value)
    }
    pub(super) fn result_head_tx(
        &mut self,
        tx: &Transaction<'_>,
        work: &str,
    ) -> Result<Option<Option<WorkCompletionRef>>> {
        let mark = self.decoder_mark();
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(head_json AS BLOB))+length(CAST(work_item_id AS BLOB))),0) FROM work_domain_result_heads WHERE agent_id=?1 AND identity_incarnation=?2 AND work_item_id=?3",
            params![self.scope.agent_id,self.scope.identity_incarnation,work],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        let nodes:usize=tx.query_row(
            "SELECT COUNT(*) FROM work_domain_result_heads c,json_tree(c.head_json) WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.work_item_id=?3",
            params![self.scope.agent_id,self.scope.identity_incarnation,work],|r|r.get(0))?;
        self.native(nodes)?;
        let value = loader::result_head_on(tx, self.scope, work)?;
        let retained_start = value
            .as_ref()
            .map(|value| -> Result<_> {
                let start = self.usage();
                // The head, heads map, selected coverage and its comparison copy
                // can coexist. JSON Strings themselves have at most two capacities.
                let bytes = usize::try_from(serialized_bytes(value)?)?
                    .checked_mul(8)
                    .and_then(|n| {
                        n.checked_add(4 * std::mem::size_of::<WorkCompletionRef>() + 1024)
                    })
                    .context("Work retained result head overflow")?;
                self.charge(0, bytes, 16)?;
                Ok(start)
            })
            .transpose()?;
        self.retire_decoder_scratch(mark, retained_start)?;
        Ok(value)
    }
    pub(super) fn manifests_tx(
        &mut self,
        tx: &Transaction<'_>,
        seed: Option<&str>,
    ) -> Result<Vec<crate::runtime_db::episode_snapshot::work_checkpoint::WorkContinuityCheckpoint>>
    {
        let Some(seed) = seed else {
            return Ok(Vec::new());
        };
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(c.capture_phase AS BLOB))+length(CAST(c.metadata_digest AS BLOB))+COALESCE(length(CAST(s.source_kind AS BLOB)),0)+COALESCE(length(CAST(CASE WHEN c.capture_phase='first_import' THEN i.roots_json ELSE s.roots_json END AS BLOB)),0)),0) FROM work_domain_captures c LEFT JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=c.seed_id WHERE c.seed_id=?1",
            [seed],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        let nodes:usize=tx.query_row(
            "SELECT COUNT(*) FROM work_domain_captures c LEFT JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=c.seed_id,json_tree(CASE WHEN c.capture_phase='first_import' THEN i.roots_json ELSE s.roots_json END) WHERE c.seed_id=?1",
            [seed],|r|r.get(0))?;
        self.native(nodes)?;
        qualification::manifests(tx, Some(seed))
    }

    /// Integer incarnation/status observation precedes any successor State read.
    /// The original decoder still validates matching identity/payload projection.
    pub(super) fn scope_tx(
        &mut self,
        tx: &Transaction<'_>,
        original_recovery: bool,
    ) -> Result<Option<ContextScopeFacts>> {
        ensure!(
            !self.scope.agent_id.is_empty() && self.scope.identity_incarnation > 0,
            "invalid Work scope"
        );
        // Incarnation lives in the canonical identity JSON, not a separate
        // SQL column. Admit that exact identity before even scalar JSON access.
        let (count,identity_bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(status AS BLOB))+length(CAST(agent_id AS BLOB))),0) FROM agent_identities WHERE agent_id=?1",
            [&self.scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(identity_bytes, count)?;
        if count == 0 {
            return Ok(None);
        }
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM agent_identities p,json_tree(p.payload_json) WHERE p.agent_id=?1",
            [&self.scope.agent_id],
            |r| r.get(0),
        )?;
        self.native(nodes)?;
        let (integer,incarnation):(bool,Option<i64>)=tx.query_row(
            "SELECT json_type(payload_json,'$.incarnation') IS NULL OR json_type(payload_json,'$.incarnation')='integer',CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 ELSE json_extract(payload_json,'$.incarnation') END FROM agent_identities WHERE agent_id=?1",
            [&self.scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            integer,
            "Work identity incarnation is neither the legacy default nor an integer"
        );
        let incarnation = incarnation.context("Work identity incarnation missing")?;
        ensure!(incarnation > 0, "Work identity incarnation invalid");
        if u64::try_from(incarnation)? != self.scope.identity_incarnation {
            return Ok(None);
        }
        let (valid_status,current_status):(bool,bool)=tx.query_row(
            "SELECT status IN ('active','deleting','deleted'),status='active' OR (?2 AND status='deleting') FROM agent_identities WHERE agent_id=?1",
            params![self.scope.agent_id,original_recovery],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(valid_status, "Work identity normalized status invalid");
        if !current_status {
            return Ok(None);
        }
        let (count,bytes):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM agent_states WHERE agent_id=?1",
            [&self.scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw(bytes, count)?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM agent_states p,json_tree(p.payload_json) WHERE p.agent_id=?1",
            [&self.scope.agent_id],
            |r| r.get(0),
        )?;
        self.native(nodes)?;
        scope_facts(tx, self.scope, original_recovery)
    }

    /// The raw predicate is the original exact record ID, with a scalar owner
    /// check before traversing another owner's body. It grants no qualification.
    pub(super) fn record_tx(
        &mut self,
        tx: &Transaction<'_>,
        key: &WorkKey,
    ) -> Result<Option<StoredRecord>> {
        let mark = self.decoder_mark();
        let (table, id) = family_table(key.family);
        let owned: Option<bool> = tx
            .query_row(
                &format!("SELECT agent_id=?2 FROM {table} WHERE {id}=?1"),
                params![key.id, self.scope.agent_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(owned) = owned else { return Ok(None) };
        ensure!(owned, "Work canonical ID belongs to another agent");
        let columns: &[&str] = match key.family {
            WorkFamily::Work => &[
                "work_item_id",
                "agent_id",
                "state",
                "revision",
                "created_at",
                "updated_at",
            ],
            WorkFamily::Wait => &[
                "wait_condition_id",
                "agent_id",
                "work_item_id",
                "status",
                "kind",
                "source",
                "subject_ref",
                "waiting_for",
                "created_at",
                "updated_at",
                "expires_at",
                "resolved_at",
                "cancelled_at",
                "last_turn_id",
                "trigger_message_id",
                "triggered_at",
                "wake_sources_json",
                "continuation_json",
            ],
            WorkFamily::Frame => &[
                "continuation_id",
                "agent_id",
                "suspended_work_item_id",
                "active_work_item_id",
                "return_policy",
                "state",
                "created_at",
                "updated_at",
                "resolved_at",
                "cancelled_at",
                "resolution_reason",
                "last_turn_id",
            ],
        };
        // Only trusted static column names enter SQL construction. Scalar SUM
        // completes before String/JSON projections or the original row decoder.
        let mut sql = format!("SELECT length(CAST(payload_json AS BLOB))+512");
        for column in columns {
            sql.push_str(&format!("+COALESCE(length(CAST({column} AS BLOB)),0)"));
        }
        sql.push_str(&format!(" FROM {table} WHERE {id}=?1"));
        let bytes: usize = tx.query_row(&sql, [&key.id], |r| r.get(0))?;
        self.raw(bytes, 1)?;
        let mut nodes: usize = tx.query_row(
            &format!("SELECT COUNT(*) FROM {table} p,json_tree(p.payload_json) WHERE p.{id}=?1"),
            [&key.id],
            |r| r.get(0),
        )?;
        if key.family == WorkFamily::Wait {
            for column in ["wake_sources_json", "continuation_json"] {
                let extra:usize=tx.query_row(&format!("SELECT COUNT(*) FROM wait_conditions p,json_tree(p.{column}) WHERE p.wait_condition_id=?1"),[&key.id],|r|r.get(0))?;
                nodes = nodes
                    .checked_add(extra)
                    .context("Wait native node count overflow")?;
            }
        }
        nodes = nodes
            .checked_add(columns.len() + 1)
            .context("Work projection node count overflow")?;
        self.native(nodes)?;
        let value = stored_on(tx, &self.scope.agent_id, key)?;
        let retained_start = value
            .as_ref()
            .map(|stored| -> Result<_> {
                let start = self.usage();
                let (body_bytes, body_records) = stored.record.resident_material_estimate()?;
                // The StoredRecord and its one qualified deep copy are the only
                // typed bodies that escape this decoder. Raw payload/fingerprint
                // Strings remain on StoredRecord; decoder Value copies are gone.
                let mut bytes = body_bytes
                    .checked_mul(4)
                    .and_then(|n| {
                        stored
                            .payload
                            .len()
                            .checked_add(stored.fingerprint.projection.len())
                            .and_then(|strings| strings.checked_mul(2))
                            .and_then(|strings| n.checked_add(strings))
                    })
                    .and_then(|n| n.checked_add(std::mem::size_of::<StoredRecord>() + 512))
                    .context("Work retained body overflow")?;
                let mut records = body_records
                    .checked_mul(2)
                    .and_then(|n| n.checked_add(8))
                    .context("Work retained body nodes overflow")?;
                // Owning keys may also enter references, pending and depth maps.
                for (_, id) in stored.record.borrowed_references().into_iter().flatten() {
                    bytes = id
                        .len()
                        .checked_add(256)
                        .and_then(|n| n.checked_mul(6))
                        .and_then(|n| bytes.checked_add(n))
                        .context("Work retained edge overflow")?;
                    records = records
                        .checked_add(6)
                        .context("Work retained edge nodes overflow")?;
                }
                self.charge(0, bytes, records)?;
                Ok(start)
            })
            .transpose()?;
        self.retire_decoder_scratch(mark, retained_start)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StateMemoryLimits;
    #[test]
    fn work_selector_material_uses_actual_ledger_and_last_guard_lifetime() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "original-work-owner".into(),
            identity_incarnation: 7,
        };
        let mut request = WorkCaptureRequest::current();
        request
            .mandatory_roots
            .insert(WorkKey::new(WorkFamily::Wait, "wait-original"));
        request.required_task_ids.insert("task-original".into());
        let guard = reserve_work_selector_material(
            &scope,
            &request,
            MAX_METADATA_RECORDS,
            MAX_METADATA_BYTES,
            &ledger,
        )?;
        let weak = Arc::downgrade(&guard);
        let clone = guard.clone();
        let observed = ledger.report()?;
        assert!(observed.observed_bytes > 0);
        assert_eq!(observed.scopes.len(), 1);
        assert_eq!(observed.scopes[0].scope, scope);
        drop(guard);
        assert!(weak.upgrade().is_some());
        assert_eq!(ledger.report()?.observed_bytes, observed.observed_bytes);
        drop(clone);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert!(
            reserve_work_selector_material(&scope, &request, 1, MAX_METADATA_BYTES, &ledger)
                .is_err()
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
