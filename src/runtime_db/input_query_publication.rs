//! The actual final Input phase owns only the next bounded query/publication copy.
use super::*;
use crate::runtime::{
    input_evidence_budget::borrowed_json_material,
    input_queries::{InputQueryCoverage, InputQueryError},
    resident_input_evidence::{OriginalInputPublicationPlan, ResidentInputEvidence},
};

/// Consumed non-Serde facts. This carrier has no old Root, Source, actor phase,
/// request loan, Runtime or DB backreference. Its guard owns the new metadata.
#[derive(Debug)]
pub(crate) struct CommittedInputQueryPublication {
    scope: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    original: OriginalInputPublicationPlan,
    future_state: [u8; 32],
    query: InputQueryCoverage,
    cost: (u64, usize),
    guard: Arc<BudgetViewGuard>,
    scratch_cost: (u64, usize),
    scratch: Arc<BudgetViewGuard>,
}
impl CommittedInputQueryPublication {
    pub(crate) fn coverage(&self) -> &InputQueryCoverage {
        &self.query
    }
    pub(crate) fn validate_original(
        &self,
        inputs: &ResidentInputEvidence,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            inputs.scope() == &self.scope
                && self.ledger.same_ledger(ledger)
                && inputs.shares_query_ledger(ledger),
            "Input publication changed scope/ledger"
        );
        self.guard
            .validate_owned_material_binding(&self.scope, ledger, self.cost.0)?;
        self.scratch
            .validate_owned_material_binding(&self.scope, ledger, self.scratch_cost.0)?;
        ensure!(
            self.original.matches(inputs)?,
            "Input publication changed original metadata before installation"
        );
        self.query.validate_selection(&Default::default())?;
        Ok(())
    }
    pub(crate) fn validate_future_state(&self, state: &crate::types::AgentState) -> Result<()> {
        ensure!(
            state.id == self.scope.agent_id
                && OriginalInputPublicationPlan::state_digest(state)? == self.future_state,
            "Input publication changed actual final State selectors"
        );
        Ok(())
    }
    pub(crate) fn copy_guard(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Arc<BudgetViewGuard>> {
        ensure!(
            scope == &self.scope && self.ledger.same_ledger(ledger),
            "Input publication copy crossed scope/ledger"
        );
        self.guard
            .validate_owned_material_binding(scope, ledger, self.cost.0)?;
        self.scratch
            .validate_owned_material_binding(scope, ledger, self.scratch_cost.0)?;
        Ok(self.scratch.clone())
    }
    pub(crate) fn metadata_guard(
        &self,
        inputs: &ResidentInputEvidence,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Arc<BudgetViewGuard>> {
        self.copy_guard(inputs.scope(), ledger)?;
        let guard = self.guard.clone();
        let actual = inputs.publication_metadata_floor()?;
        ensure!(
            actual.0 <= self.cost.0 && actual.1 <= self.cost.1,
            InputQueryError::ResourceExhausted("new publication metadata")
        );
        Ok(guard)
    }
}
struct BorrowedBorn<'a>(&'a CommittedInputQueryAdvance);
impl serde::Serialize for BorrowedBorn<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.born.iter())
    }
}
struct BorrowedDelta<'a>(&'a CommittedInputEvidenceDelta);
impl serde::Serialize for BorrowedDelta<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        struct Locators<'a>(&'a CommittedInputEvidenceDelta);
        impl serde::Serialize for Locators<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.collect_seq(self.0.records.iter().map(|record| &record.locator))
            }
        }
        struct Removed<'a>(&'a CommittedInputEvidenceDelta);
        impl serde::Serialize for Removed<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.collect_seq(self.0.removals.iter().map(|row| {
                    (
                        &row.scope,
                        &row.key,
                        &row.revision,
                        &row.previous_content_digest,
                    )
                }))
            }
        }
        let delta = self.0;
        let execution = &delta.execution;
        let mut tuple = serializer.serialize_tuple(5)?;
        tuple.serialize_element(&Locators(delta))?;
        tuple.serialize_element(&Removed(delta))?;
        tuple.serialize_element(&(
            &execution.initialized,
            &execution.sources,
            &execution.open_attempts,
            &execution.nonterminal_work_items,
            &execution.work_items,
        ))?;
        tuple.serialize_element(&delta.message_turn_keys)?;
        tuple.serialize_element(&delta.turn_transcript_keys)?;
        tuple.end()
    }
}

pub(super) fn prepare_tx(
    tx: &Transaction<'_>,
    advance: &CommittedInputQueryAdvance,
    delta: &CommittedInputEvidenceDelta,
    source: &crate::runtime::mutation_owner::OriginalInputQuerySource,
    ledger: &InputEvidenceBudgetLedger,
    material: &mut read_material::InputReadMaterial<'_>,
    future_state: &crate::types::AgentState,
) -> Result<Option<CommittedInputQueryPublication>> {
    let scope = &advance.scope;
    material.validate_accounting(scope, ledger)?;
    source
        .roles()
        .validate_binding(scope, source.owner_epoch(), ledger)?;
    let Some(base) = source.roles().original_base() else {
        return Ok(None);
    };
    if base.birth_cursor != Some(advance.base_cursor) {
        return Ok(None);
    }
    let selection = &source.roles().selection;
    if !selection
        .required_keys
        .iter()
        .all(|key| base.required_keys.contains(key))
    {
        return Ok(None);
    }
    if let Some(after) = selection.birth_after {
        if after > advance.base_cursor
            || (after < advance.base_cursor && !base.birth_after.is_some_and(|cut| cut <= after))
        {
            return Ok(None);
        }
        let expected = advance
            .base_cursor
            .checked_sub(after)
            .context(InputQueryError::Incomplete)?;
        let actual: u64=tx.query_row(
            "SELECT COUNT(*) FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3 AND birth_seq<=?4",
            params![scope.agent_id,scope.identity_incarnation,after,advance.base_cursor],|row|row.get(0))?;
        if actual != expected
            || u64::try_from(
                base.born_key_seqs
                    .values()
                    .filter(|seq| **seq > after)
                    .count(),
            )? != expected
        {
            return Ok(None);
        }
        for (key, seq) in base.born_key_seqs.iter().filter(|(_, seq)| **seq > after) {
            let exact:bool=tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 AND birth_seq=?5)",
                params![scope.agent_id,scope.identity_incarnation,key.family.as_str(),key.id,seq],|row|row.get(0))?;
            if !exact {
                return Ok(None);
            }
        }
    }
    // Source completion requires the finite actual Tool report, not its stdout.
    for record in &delta.records {
        if record.locator.key.family == Family::ToolExecution {
            if record.tool_run_report.is_none() {
                return Ok(None);
            }
            record.tool_run_report()?;
        }
    }
    let Some(plan) =
        source
            .roles()
            .original_publication_copy_plan(scope, source.owner_epoch(), ledger)?
    else {
        return Ok(None);
    };
    ensure!(
        future_state.id == scope.agent_id,
        "Input publication final State scope differs"
    );
    let (raw, native) = borrowed_json_material(&(
        scope,
        base,
        selection,
        BorrowedBorn(advance),
        BorrowedDelta(delta),
    ))?;
    let state = OriginalInputPublicationPlan::state_material(future_state)?;
    let raw = raw
        .checked_add(state.0)
        .context(InputQueryError::ResourceExhausted("final State selectors"))?;
    let native = native
        .checked_add(state.1)
        .context(InputQueryError::ResourceExhausted("final State selectors"))?;
    let mut edge_bytes = 0u64;
    let mut edge_nodes = 0usize;
    let mut overflow = false;
    for record in &delta.records {
        record.visit_references(|_, id| {
            match edge_bytes
                .checked_add(id.len() as u64)
                .and_then(|n| n.checked_add(128))
            {
                Some(next) => edge_bytes = next,
                None => overflow = true,
            }
            match edge_nodes.checked_add(6) {
                Some(next) => edge_nodes = next,
                None => overflow = true,
            }
        });
    }
    ensure!(
        !overflow,
        InputQueryError::ResourceExhausted("final publication delta")
    );
    // Existing forecast covers the old metadata and closure work. Eight copies
    // of new variable keys cover the capsule, installed map and selector work;
    // qualified body allocations stay shared and are never serialized here.
    let added_nodes = native
        .checked_add(edge_nodes)
        .and_then(|n| n.checked_add(32))
        .context(InputQueryError::ResourceExhausted(
            "final publication delta",
        ))?;
    let added_bytes = raw
        .checked_add(edge_bytes)
        .and_then(|n| n.checked_add(4096))
        .and_then(|n| n.checked_add(u64::try_from(added_nodes).ok()?.checked_mul(256)?))
        .context(InputQueryError::ResourceExhausted(
            "final publication delta",
        ))?;
    let old = plan.material_estimate();
    let retained = plan.retained_copy_estimate();
    let retained = (
        retained.0.checked_add(added_bytes),
        retained.1.checked_add(added_nodes),
    );
    let (Some(retained_bytes), Some(retained_nodes)) = retained else {
        anyhow::bail!(InputQueryError::ResourceExhausted(
            "new publication metadata"
        ));
    };
    let old_floor = plan.retained_copy_estimate();
    let scratch_bytes = old
        .0
        .checked_sub(old_floor.0)
        .and_then(|n| n.checked_add(added_bytes.checked_mul(7)?))
        .context(InputQueryError::ResourceExhausted(
            "publication copy scratch",
        ))?;
    let scratch_nodes = old
        .1
        .checked_sub(old_floor.1)
        .and_then(|n| n.checked_add(added_nodes.checked_mul(7)?))
        .context(InputQueryError::ResourceExhausted(
            "publication copy scratch",
        ))?;
    // Both allocations enter the original Input/Memory phase before copying.
    // The next Root keeps only its one-copy guard; ACK owns transient scratch.
    let (scratch, scratch_actual) =
        material.observe_retained_projection(scratch_bytes, scratch_nodes)?;
    let (guard, actual) = material.observe_retained_projection(retained_bytes, retained_nodes)?;
    let after = selection.birth_after;
    let mut query = InputQueryCoverage {
        required_keys: selection.required_keys.clone(),
        birth_cursor: Some(advance.cursor),
        birth_after: after,
        born_key_seqs: BTreeMap::new(),
    };
    if let Some(after) = after {
        query.born_key_seqs.extend(
            base.born_key_seqs
                .iter()
                .filter(|(_, seq)| **seq > after)
                .map(|(key, seq)| (key.clone(), *seq)),
        );
        for (key, (seq, _)) in &advance.born {
            query.born_key_seqs.insert(key.clone(), *seq);
        }
        ensure!(
            u64::try_from(query.born_key_seqs.len())? == advance.cursor - after,
            InputQueryError::Incomplete
        );
    }
    query.validate_selection(selection)?;
    Ok(Some(CommittedInputQueryPublication {
        scope: scope.clone(),
        ledger: ledger.clone(),
        original: plan,
        query,
        future_state: OriginalInputPublicationPlan::state_digest(future_state)?,
        cost: (u64::try_from(actual.0)?, actual.1),
        guard,
        scratch_cost: (u64::try_from(scratch_actual.0)?, scratch_actual.1),
        scratch,
    }))
}

/// Test the actual Native/D3 kernel and final admission in one concrete SQL TX.
#[cfg(test)]
pub(crate) fn exercise_original_publication_for_fixture<T>(
    db: &crate::runtime_db::RuntimeDb,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    source: &Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>,
    request: &InputEvidenceLoadRequest,
    message: &crate::types::MessageEnvelope,
    state: &crate::types::AgentState,
    action: impl FnOnce((usize, usize), &CommittedInputEvidenceDelta) -> Result<T>,
) -> Result<T> {
    db.transaction_once(|tx| {
        let mut material = read_material::InputReadMaterial::new_with_memory_and_query_prior(
            scope,
            request,
            ledger,
            (0, 0),
            None,
            Some((source.roles(), source.owner_epoch())),
        )?;
        let native = NativeInputWriteProof::prepare_using_input_material_tx(
            tx,
            scope,
            |visit| visit(Family::Message, &message.id),
            &mut material,
            Some(source),
        )?
        .context("actual Native source missing")?;
        let (raw, nodes) = borrowed_json_material(message)?;
        material.observe_projected(
            raw.checked_mul(8)
                .context("fixture body bytes overflow")?
                .checked_add(
                    u64::try_from(nodes)?
                        .checked_mul(8 * 256)
                        .context("fixture body native overflow")?,
                )
                .context("fixture body bytes overflow")?,
            nodes
                .checked_mul(8)
                .context("fixture body nodes overflow")?,
        )?;
        let native = native.retain_input_phase(material)?;
        let (actual, inserted) = crate::runtime_db::evidence::append_message_tx(tx, message)?;
        ensure!(inserted, "actual fixture Message unexpectedly existed");
        let mut delta = register_committed_input_evidence_tx(
            tx,
            scope,
            &[CanonicalInputEvidence::Message(actual)],
        )?;
        let before = native
            .input_phase_usage
            .context("actual fixture phase missing")?;
        native.finish_delta_with_publication_tx(tx, &mut delta, ledger, None, state)?;
        action(before, &delta)
    })
}
#[cfg(test)]
impl CommittedInputQueryPublication {
    pub(crate) fn cost_for_fixture(&self) -> (u64, usize) {
        (
            self.cost.0 + self.scratch_cost.0,
            self.cost.1 + self.scratch_cost.1,
        )
    }
    pub(crate) fn weak_guard_for_fixture(&self) -> std::sync::Weak<BudgetViewGuard> {
        Arc::downgrade(&self.guard)
    }
    pub(crate) fn weak_scratch_for_fixture(&self) -> std::sync::Weak<BudgetViewGuard> {
        Arc::downgrade(&self.scratch)
    }
}
