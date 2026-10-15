//! Closed original decoders retire only scratch after their returned DTO exists.
use super::*;
#[path = "constructor_material.rs"]
mod constructor_material;
use crate::runtime::input_evidence_budget::borrowed_json_material;

struct TaskDecoderMark {
    scope: usize,
    request: usize,
    ledger: InputEvidenceBudgetLedger,
    anchor: Option<std::sync::Weak<BudgetViewGuard>>,
    bytes: usize,
    records: usize,
    guards: usize,
}
struct RetainedTaskDecoder {
    scope: usize,
    request: usize,
    ledger: InputEvidenceBudgetLedger,
    guard: std::sync::Weak<BudgetViewGuard>,
    bytes: usize,
    records: usize,
}
pub(in crate::runtime_db::task_snapshot) struct CurrentTaskMetadataRead {
    value: Option<TaskCoverage>,
    retained: Option<RetainedTaskDecoder>,
}
impl CurrentTaskMetadataRead {
    pub(in crate::runtime_db::task_snapshot) fn as_ref(&self) -> Option<&TaskCoverage> {
        self.value.as_ref()
    }
    pub(in crate::runtime_db::task_snapshot) fn as_mut(&mut self) -> Option<&mut TaskCoverage> {
        self.value.as_mut()
    }
    pub(in crate::runtime_db::task_snapshot) fn retire(
        self,
        material: &mut TaskReadMaterial<'_>,
    ) -> Result<()> {
        let Self { value, retained } = self;
        drop(value);
        if let Some(retained) = retained {
            material.release_current_metadata(retained)?;
        }
        Ok(())
    }
}
impl TaskReadMaterial<'_> {
    fn decoder_mark(&self) -> Result<TaskDecoderMark> {
        Ok(TaskDecoderMark {
            scope: self.scope as *const InputEvidenceScope as usize,
            request: self.request as *const TaskDomainLoadRequest as usize,
            ledger: self
                .budget
                .context("Task decoder requires actual ledger")?
                .clone(),
            anchor: self.guards.first().map(Arc::downgrade),
            bytes: self.bytes,
            records: self.records,
            guards: self.guards.len(),
        })
    }
    fn compact_decoder(
        &mut self,
        mark: TaskDecoderMark,
        bytes: u64,
        nodes: usize,
    ) -> Result<Option<RetainedTaskDecoder>> {
        ensure!(
            mark.scope == self.scope as *const InputEvidenceScope as usize
                && mark.request == self.request as *const TaskDomainLoadRequest as usize
                && self
                    .budget
                    .is_some_and(|ledger| ledger.same_ledger(&mark.ledger))
                && self.guards.len() >= mark.guards
                && mark
                    .anchor
                    .as_ref()
                    .is_none_or(|anchor| self.guards.first().is_some_and(|guard| {
                        std::sync::Weak::ptr_eq(anchor, &Arc::downgrade(guard))
                    })),
            "Task decoder changed original phase"
        );
        ensure!(
            self.guards[mark.guards..]
                .iter()
                .all(|guard| Arc::strong_count(guard) == 1),
            "Task decoder guard has another live holder"
        );
        for guard in &self.guards[mark.guards..] {
            guard.validate_owned_material_binding(self.scope, &mark.ledger, 0)?;
        }
        // Four slots and 512 bytes own this guard, its ledger scope and token.
        let nodes = if bytes == 0 && nodes == 0 {
            0
        } else {
            nodes
                .checked_add(4)
                .context("Task retained decoder slots overflow")?
        };
        let retained = if nodes == 0 {
            0
        } else {
            usize::try_from(bytes)?
                .checked_add(self.scope.agent_id.len())
                .and_then(|n| n.checked_add(512))
                .and_then(|n| {
                    nodes
                        .checked_mul(256)
                        .and_then(|native| n.checked_add(native))
                })
                .context("Task retained decoder bytes overflow")?
        };
        let suffix = (
            self.bytes
                .checked_sub(mark.bytes)
                .context("Task decoder bytes underflow")?,
            self.records
                .checked_sub(mark.records)
                .context("Task decoder nodes underflow")?,
        );
        ensure!(
            retained <= suffix.0 && nodes <= suffix.1,
            TaskSnapshotError::ResourceExhausted(
                "Task retained decoder result exceeds admitted peak"
            )
        );
        let next = (
            mark.bytes
                .checked_add(retained)
                .context("Task decoder total bytes overflow")?,
            mark.records
                .checked_add(nodes)
                .context("Task decoder total nodes overflow")?,
        );
        bound(next.1, next.0, Some(self.request))?;
        let guard = if retained == 0 {
            None
        } else {
            Some(mark.ledger.register_owned_bytes(
                self.scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: u64::try_from(retained)?,
                },
            )?)
        };
        if let Some(memory) = self.memory_budget.as_deref_mut() {
            memory.release_task_readback_material(
                ReleasedTaskReadbackMaterial::completed_decoder(
                    self.scope,
                    &mark.ledger,
                    suffix.0 - retained,
                    suffix.1 - nodes,
                ),
            )?;
        }
        let token = guard.as_ref().map(|guard| RetainedTaskDecoder {
            scope: mark.scope,
            request: mark.request,
            ledger: mark.ledger.clone(),
            guard: Arc::downgrade(guard),
            bytes: retained,
            records: nodes,
        });
        self.guards.truncate(mark.guards);
        self.guards.extend(guard);
        self.bytes = next.0;
        self.records = next.1;
        Ok(token)
    }
    // Only CurrentTaskMetadataRead can consume this token, after dropping its DTO.
    fn release_current_metadata(&mut self, retained: RetainedTaskDecoder) -> Result<()> {
        ensure!(
            retained.scope == self.scope as *const InputEvidenceScope as usize
                && retained.request == self.request as *const TaskDomainLoadRequest as usize
                && self
                    .budget
                    .is_some_and(|ledger| ledger.same_ledger(&retained.ledger)),
            "Task current metadata belongs to another phase"
        );
        let index = self
            .guards
            .iter()
            .position(|guard| std::sync::Weak::ptr_eq(&retained.guard, &Arc::downgrade(guard)))
            .context("Task current metadata guard no longer belongs to this phase")?;
        let guard = &self.guards[index];
        ensure!(
            Arc::strong_count(guard) == 1,
            "Task current metadata guard has another live holder"
        );
        guard.validate_owned_material_binding(
            self.scope,
            &retained.ledger,
            u64::try_from(retained.bytes)?,
        )?;
        let next = (
            self.bytes
                .checked_sub(retained.bytes)
                .context("Task current metadata bytes underflow")?,
            self.records
                .checked_sub(retained.records)
                .context("Task current metadata nodes underflow")?,
        );
        if let Some(memory) = self.memory_budget.as_deref_mut() {
            memory.release_task_readback_material(
                ReleasedTaskReadbackMaterial::completed_decoder(
                    self.scope,
                    &retained.ledger,
                    retained.bytes,
                    retained.records,
                ),
            )?;
        }
        self.guards.remove(index);
        self.bytes = next.0;
        self.records = next.1;
        Ok(())
    }
    pub(in crate::runtime_db::task_snapshot) fn original_current_metadata_tx(
        &mut self,
        tx: &Transaction<'_>,
    ) -> Result<CurrentTaskMetadataRead> {
        if self.budget.is_none() {
            return Ok(CurrentTaskMetadataRead {
                value: self.read_current_metadata(tx)?,
                retained: None,
            });
        }
        let mark = self.decoder_mark()?;
        // Original raw four-buffer gate and complete nodes/normalized checks.
        // current_coverage creates one typed DTO and no generic JSON Value.
        let value = self.read_current_metadata_for_scope_inner(tx, self.scope, 1)?;
        let (bytes, nodes) = match &value {
            Some(value) => borrowed_json_material(value)?,
            None => (0, 0),
        };
        let retained = self.compact_decoder(mark, bytes, nodes)?;
        Ok(CurrentTaskMetadataRead { value, retained })
    }
    pub(in crate::runtime_db::task_snapshot) fn original_stored_row_tx(
        &mut self,
        tx: &Transaction<'_>,
        agent: &str,
        key: &TaskKey,
        before_decode: impl FnOnce(Option<usize>) -> Result<()>,
    ) -> Result<Option<StoredTask>> {
        if self.budget.is_none() {
            let size = self.admit_stored_tx(tx, agent, key)?;
            before_decode(size)?;
            return stored_on(tx, agent, key);
        }
        let mark = self.decoder_mark()?;
        let size = self.admit_stored_tx(tx, agent, key)?;
        before_decode(size)?;
        let item = stored_on(tx, agent, key)?;
        let (bytes, nodes) = match &item {
            // Raw payload, typed fact and normalized digest are still distinct.
            Some(item) => {
                borrowed_json_material(&(&item.payload, &item.fact, &item.projection_digest))?
            }
            None => (0, 0),
        };
        let _ = self.compact_decoder(mark, bytes, nodes)?;
        Ok(item)
    }
    pub(in crate::runtime_db::task_snapshot) fn admit_original_written_keys(
        &mut self,
        tx: &Transaction<'_>,
        agent: &str,
        keys: &BTreeSet<TaskKey>,
    ) -> Result<()> {
        self.borrowed_loader_material(keys, 2, 1)?;
        // Preserve the original all-key validation order before the first alias
        // writer. Each scalar traversal exits; no row DTO exists in this pass.
        if self.budget.is_some() && keys.len() > 1 {
            for key in keys {
                let mark = self.decoder_mark()?;
                self.admit_stored_tx(tx, agent, key)?;
                let _ = self.compact_decoder(mark, 0, 0)?;
            }
        }
        Ok(())
    }
    // The actual alias/query writer returns only scalar change/removal facts.
    pub(in crate::runtime_db::task_snapshot) fn with_original_written_row(
        &mut self,
        tx: &Transaction<'_>,
        agent: &str,
        key: &TaskKey,
        write: impl FnOnce(Option<StoredTask>) -> Result<(bool, bool)>,
    ) -> Result<(bool, bool)> {
        if self.budget.is_none() {
            return write(stored_on(tx, agent, key)?);
        }
        let mark = self.decoder_mark()?;
        let sizing = self.decoder_mark()?;
        let columns = self.original_written_columns_tx(tx, agent, key)?;
        // Only three scalar lengths escape. The complete SQLite sizing
        // statement/column references have exited before the typed decoder.
        let _ = self.compact_decoder(sizing, 0, 0)?;
        if let Some(columns) = columns {
            self.admit_original_written_decode_tx(tx, key, columns)?;
        }
        let result = write(stored_on(tx, agent, key)?)?;
        // write consumed/dropped the row; only two booleans escape.
        let _ = self.compact_decoder(mark, 0, 0)?;
        Ok(result)
    }
    pub(in crate::runtime_db::task_snapshot) fn original_qualified_row(
        &mut self,
        item: &StoredTask,
        scope: &InputEvidenceScope,
        revision: u64,
    ) -> Result<Arc<QualifiedTaskFact>> {
        if self.budget.is_none() {
            return item.qualified(scope, revision);
        }
        let mark = self.decoder_mark()?;
        let (bytes, nodes) = borrowed_json_material(&(scope, &item.fact))?;
        // New fact + encoded sizing + validation coexist; Stored is in prefix.
        self.charge(
            usize::try_from(bytes)?
                .checked_add(1024)
                .context("Task qualified sizing overflow")?,
            nodes
                .checked_add(16)
                .context("Task qualified slots overflow")?,
            3,
            false,
        )?;
        let record = item.qualified(scope, revision)?;
        let (bytes, nodes) = borrowed_json_material(&(&record.locator, &record.fact))?;
        let _ = self.compact_decoder(
            mark,
            bytes.max(u64::try_from(record.resident_bytes)?),
            nodes,
        )?;
        Ok(record)
    }
    fn borrowed_loader_material(
        &mut self,
        value: &impl serde::Serialize,
        slots: usize,
        copies: usize,
    ) -> Result<()> {
        let (bytes, nodes) = borrowed_json_material(value)?;
        self.charge(
            usize::try_from(bytes)?,
            nodes
                .checked_add(slots)
                .context("Task loader slots overflow")?,
            copies,
            true,
        )
    }
    pub(in crate::runtime_db::task_snapshot) fn admit_loader_required(
        &mut self,
        keys: &BTreeSet<TaskKey>,
    ) -> Result<()> {
        // The keys builder and the independently owned coverage required set.
        self.borrowed_loader_material(keys, 2, 2)
    }
    pub(in crate::runtime_db::task_snapshot) fn admit_loader_key_copies(
        &mut self,
        keys: &BTreeSet<TaskKey>,
    ) -> Result<()> {
        // Pending keys become Stored map keys; observation keys are separate.
        self.borrowed_loader_material(keys, 3, 2)
    }
    pub(in crate::runtime_db::task_snapshot) fn admit_loader_metadata(
        &mut self,
        item: &StoredTask,
    ) -> Result<()> {
        match &item.fact {
            CanonicalTaskFact::Task(task) => {
                let detail = task.detail.as_ref().and_then(serde_json::Value::as_object);
                let obligation = detail
                    .and_then(|value| value.get("rejoin_obligation_id"))
                    .and_then(serde_json::Value::as_str);
                let turn = detail
                    .and_then(|value| value.get("parent_turn_id"))
                    .and_then(serde_json::Value::as_str);
                let generation = detail
                    .and_then(|value| value.get("rejoin_generation"))
                    .and_then(serde_json::Value::as_u64);
                // Exact variable fields used by from_record, including rejoin.
                self.borrowed_loader_material(
                    &(
                        &task.id,
                        task.kind.as_str(),
                        &task.status,
                        task.created_at,
                        task.updated_at,
                        &task.work_item_id,
                        &task.parent_message_id,
                        obligation,
                        turn,
                        generation,
                    ),
                    18,
                    1,
                )?;
                // Map keys, roots and the second short-lived rejoin fence.
                self.borrowed_loader_material(
                    &(
                        &task.id,
                        &task.id,
                        &task.work_item_id,
                        &task.parent_message_id,
                        obligation,
                        turn,
                    ),
                    10,
                    1,
                )
            }
            CanonicalTaskFact::ResultSettlement(result) => {
                self.borrowed_loader_material(
                    &(
                        &result.result_identity,
                        &result.task_id,
                        &result.message_id,
                        &result.work_item_id,
                        result.rejoin_generation,
                        &result.parent_turn_id,
                        result.state,
                        &result.activation_id,
                        result.created_at,
                        result.next_recheck_at,
                    ),
                    20,
                    1,
                )?;
                self.borrowed_loader_material(
                    &(
                        &result.result_identity,
                        &result.result_identity,
                        &result.result_identity,
                        &result.result_identity,
                        &result.task_id,
                        &result.activation_id,
                        &result.message_id,
                        &result.parent_turn_id,
                        &result.work_item_id,
                    ),
                    14,
                    1,
                )
            }
        }
    }
    pub(in crate::runtime_db::task_snapshot) fn with_original_resident_ctor(
        &mut self,
        scope: InputEvidenceScope,
        coverage: TaskCoverage,
        records: Vec<Arc<QualifiedTaskFact>>,
        stops: Vec<Arc<crate::runtime_db::task_stop::TaskStopRecord>>,
        budget: Option<&InputEvidenceBudgetLedger>,
    ) -> Result<ResidentTaskDomain> {
        if self.budget.is_none() {
            return ResidentTaskDomain::bootstrap_with_stops(
                scope,
                coverage,
                records,
                stops,
                self.request.record_limit,
                self.request.byte_limit,
            );
        }
        // Resident scope/map keys and Arc slots survive constructor validation.
        self.borrowed_loader_material(&scope, 4, 1)?;
        for record in &records {
            self.borrowed_loader_material(&record.locator.key, 3, 1)?;
        }
        self.charge(
            0,
            stops
                .len()
                .checked_add(4)
                .context("Task resident slots overflow")?,
            1,
            true,
        )?;
        let mark = self.decoder_mark()?;
        // Coverage moves into the resident. Only root/pending/related keys and
        // validation pointer/sequence sets are copied by the constructor.
        self.borrowed_loader_material(
            &constructor_material::BorrowedKeys(&coverage),
            records.len().checked_add(stops.len())
                .and_then(|n| n.checked_add(coverage.stop.records.len()))
                .and_then(|n| n.checked_add(coverage.stop.pending_effect_seqs.len()))
                .and_then(|n| n.checked_add(coverage.terminal.keys.len()))
                .context("Task ctor slots overflow")?,
            3,
        )?;
        let mut max =
            records
                .iter()
                .try_fold((0u64, 0usize), |(bytes, nodes), record| -> Result<_> {
                    let actual = borrowed_json_material(&(&record.locator, &record.fact))?;
                    Ok((bytes.max(actual.0), nodes.max(actual.1)))
                })?;
        for record in &stops {
            let actual = borrowed_json_material(record.as_ref())?;
            max = (max.0.max(actual.0), max.1.max(actual.1));
        }
        // One record's encoded validation/metadata temporary exists at a time.
        self.charge(
            usize::try_from(max.0)?
                .checked_add(1024)
                .context("Task ctor validation bytes overflow")?,
            max.1
                .checked_add(16)
                .context("Task ctor validation slots overflow")?,
            2,
            false,
        )?;
        let resident = ResidentTaskDomain::bootstrap_with_stops(
            scope,
            coverage,
            records,
            stops,
            self.request.record_limit,
            self.request.byte_limit,
        )?;
        let resident = match budget {
            Some(budget) => resident.with_budget(budget.clone())?,
            None => resident,
        };
        // Only temporary roots/validation exit. Resident coverage/body guards
        // were independently admitted before the first allocation above.
        let _ = self.compact_decoder(mark, 0, 0)?;
        Ok(resident)
    }
}

#[cfg(test)]
#[path = "decoder_material_tests.rs"]
mod tests;
