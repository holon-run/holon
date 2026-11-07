//! Original startup/Control Wait cancellation, preserving published history.

use super::*;
use crate::types::{WaitConditionRecord, WaitConditionStatus, WakeSource};

pub(crate) fn prepare_work_wait_cleanup_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    wait_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
) -> Result<WorkWriteProof> {
    prepare_cleanup(tx, scope, wait_ids, request, false)
}

pub(crate) fn prepare_original_work_wait_cleanup_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    wait_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
) -> Result<WorkWriteProof> {
    prepare_cleanup(tx, scope, wait_ids, request, true)
}

fn prepare_cleanup(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    wait_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
    original: bool,
) -> Result<WorkWriteProof> {
    // Only existing owning closure is admitted. No optional history body is
    // read merely because a maintenance operation needs to cancel a Wait.
    let mut pending = wait_ids
        .iter()
        .map(|id| (WorkKey::new(WorkFamily::Wait, id), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut keys = BTreeSet::new();
    let mut body_bytes = 0usize;
    let mut key_bytes = 0usize;
    while let Some((key, depth)) = pending.pop_first() {
        if !keys.insert(key.clone()) {
            continue;
        }
        key_bytes = key_bytes.saturating_add(key.id.len());
        bound_metadata(keys.len(), key_bytes)?;
        ensure!(
            depth <= MAX_CLOSURE_DEPTH,
            "Wait cleanup owning closure depth exhausted"
        );
        ensure!(
            keys.len() <= request.record_limit,
            "Wait cleanup record budget exhausted"
        );
        let (table, id) = family_table(key.family);
        let bytes: usize = tx.query_row(
            &format!("SELECT length(CAST(payload_json AS BLOB)) FROM {table} WHERE {id}=?1"),
            [&key.id],
            |row| row.get(0),
        )?;
        body_bytes = body_bytes.saturating_add(bytes);
        ensure!(
            body_bytes <= request.byte_limit,
            "Wait cleanup body budget exhausted"
        );
        let record =
            stored_on(tx, &scope.agent_id, &key)?.context("Wait cleanup owning body missing")?;
        for child in record.record.references() {
            if !keys.contains(&child) {
                pending.entry(child).or_insert(depth + 1);
            }
        }
    }
    if original {
        prepare_original_work_write_tx(tx, scope, &keys)
    } else {
        let proof = prepare_work_write_tx(tx, scope, &keys)?;
        ensure!(
            proof.before.values().all(Option::is_some),
            "Wait cleanup cannot create a Work key"
        );
        Ok(proof)
    }
}

#[cfg(test)]
pub(crate) fn register_work_wait_cleanup_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    writes: &repositories::WaitConditionWrites,
    final_state: &AgentState,
    request: &WorkDomainLoadRequest,
) -> Result<CommittedWorkFacts> {
    register_wait_cleanup_ids_tx(
        tx,
        proof,
        writes.wait_ids.iter(),
        Some(final_state),
        request,
    )?
    .context("Control Wait cleanup must return its final domain facts")
}

/// Startup may clean an original Wait before an AgentState was materialized.
fn register_wait_cleanup_ids_tx<'a>(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    wait_ids: impl Iterator<Item = &'a String>,
    final_state: Option<&AgentState>,
    request: &WorkDomainLoadRequest,
) -> Result<Option<CommittedWorkFacts>> {
    let facts = scope_facts(tx, &proof.scope, proof.original_recovery)?
        .context("Wait cleanup identity retired")?;
    ensure!(
        facts.state.as_deref() == final_state,
        "Wait cleanup State epilogue differs"
    );
    let original = loader::current_coverage(tx, &proof.scope)?;
    ensure!(
        original.as_ref().map_or(1, |coverage| coverage.revision) == proof.base_revision,
        "Wait cleanup coverage changed since preparation"
    );
    let actual = wait_ids
        .map(|id| WorkKey::new(WorkFamily::Wait, id))
        .collect::<BTreeSet<_>>();
    for key in &actual {
        let before = proof
            .before
            .get(key)
            .and_then(Option::as_ref)
            .context("Wait cleanup actual ID has no original qualified proof")?;
        let CanonicalWorkRecord::Wait(prior) = &before.record else {
            anyhow::bail!("Wait cleanup proof is not a Wait")
        };
        let after =
            stored_on(tx, &proof.scope.agent_id, key)?.context("Wait cleanup removed its body")?;
        let CanonicalWorkRecord::Wait(wait) = &after.record else {
            unreachable!()
        };
        ensure!(
            wait.work_item_id == prior.work_item_id
                && wait.status == WaitConditionStatus::Cancelled,
            "Wait cleanup changed ownership or did not cancel the original Wait"
        );
    }
    // A cold scope has no published history to retain. Prepare its finite
    // current closure, without claiming an empty complete resident domain.
    if original
        .as_ref()
        .is_none_or(|coverage| !coverage.live_complete)
    {
        let Some(final_state) = final_state else {
            ensure!(
                original.is_none(),
                "State-less startup cannot replace incomplete Work coverage"
            );
            register_state_less_waits(tx, proof, &actual, request)?;
            return Ok(None);
        };
        let mut current = request.clone();
        current.capture.nonopen_limit = 0;
        current
            .capture
            .mandatory_roots
            .extend(actual.iter().cloned());
        return register_work_writes_tx(
            tx,
            proof,
            &actual,
            &BTreeSet::new(),
            final_state,
            &current,
        )
        .map(Some);
    }
    let mut coverage = original.expect("checked complete coverage");
    let revision = proof
        .base_revision
        .checked_add(1)
        .context("Work revision overflow")?;
    let mut records = BTreeMap::<WorkKey, Arc<QualifiedWorkRecord>>::new();
    let mut body_bytes = 0usize;
    let mut owning_roots = BTreeSet::new();
    for key in &actual {
        let before = proof
            .before
            .get(key)
            .and_then(Option::as_ref)
            .context("Wait cleanup actual ID has no original qualified proof")?;
        let CanonicalWorkRecord::Wait(prior) = &before.record else {
            anyhow::bail!("Wait cleanup proof is not a Wait")
        };
        let after =
            stored_on(tx, &proof.scope.agent_id, key)?.context("Wait cleanup removed its body")?;
        let CanonicalWorkRecord::Wait(wait) = &after.record else {
            unreachable!()
        };
        ensure!(
            wait.work_item_id == prior.work_item_id
                && wait.status == WaitConditionStatus::Cancelled,
            "Wait cleanup changed ownership or did not cancel the original Wait"
        );
        ensure!(
            !alias(tx, &proof.scope, key)?
                .is_some_and(|alias| alias.availability == WorkAvailability::Pruned),
            "Wait cleanup resurrected a pruned record"
        );
        if before.payload == after.payload && before.fingerprint == after.fingerprint {
            continue;
        }
        body_bytes = body_bytes.saturating_add(after.payload.len());
        ensure!(
            body_bytes <= request.byte_limit,
            "Wait cleanup ACK body budget exhausted"
        );
        upsert_alias(
            tx,
            &proof.scope,
            key,
            &Alias {
                revision,
                availability: WorkAvailability::Present,
                digest: Some(after.digest()),
                fingerprint: Some(after.fingerprint.clone()),
            },
        )?;
        relations::register_wait_edges_tx(tx, &proof.scope, wait, revision, false)?;
        update_wait_membership(&mut coverage, wait);
        coverage
            .observations
            .insert(key.clone(), WorkAvailability::Present);
        coverage.mandatory_roots.insert(key.clone());
        if let Some(id) = &wait.trigger_message_id {
            owning_roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Message, id));
        }
        records.insert(key.clone(), after.qualified(&proof.scope, revision)?);
        // Unselected fixed owners may become necessary only now. Qualify their
        // original observed bodies, never a later raw history row.
        let mut pending = after.record.references();
        while let Some(child) = pending.pop_first() {
            if coverage.observations.get(&child) == Some(&WorkAvailability::Present) {
                continue;
            }
            let stored = proof
                .before
                .get(&child)
                .and_then(Option::as_ref)
                .context("Wait cleanup owning dependency omitted from original proof")?;
            let current = stored_on(tx, &proof.scope.agent_id, &child)?
                .context("Wait cleanup owner disappeared")?;
            ensure!(
                current.payload == stored.payload && current.fingerprint == stored.fingerprint,
                "Wait cleanup owner changed in the original transaction"
            );
            let version = qualification::validate_member(
                tx,
                &proof.scope,
                binding(tx, &proof.scope)?.as_deref(),
                &child,
                &current,
            )?;
            body_bytes = body_bytes.saturating_add(current.payload.len());
            ensure!(
                body_bytes <= request.byte_limit && records.len() < request.record_limit,
                "Wait cleanup owning ACK budget exhausted"
            );
            upsert_alias(
                tx,
                &proof.scope,
                &child,
                &Alias {
                    revision: version,
                    availability: WorkAvailability::Present,
                    digest: Some(current.digest()),
                    fingerprint: Some(current.fingerprint.clone()),
                },
            )?;
            match &current.record {
                CanonicalWorkRecord::Work(work) => {
                    ensure!(
                        coverage.live_work_ids.contains(&work.id)
                            == matches!(
                                work.state,
                                crate::types::WorkItemState::Open
                                    | crate::types::WorkItemState::Completing
                            ),
                        "Wait cleanup cannot invent complete live Work membership"
                    );
                    // A fixed owner's exact head is already durable, and is
                    // read by key rather than rediscovered from Brief history.
                    if !coverage.latest_results.contains_key(&work.id) {
                        let mut head = loader::result_head_on(tx, &proof.scope, &work.id)?;
                        if head.is_none() {
                            for manifest in
                                qualification::manifests(tx, binding(tx, &proof.scope)?.as_deref())?
                            {
                                if let Some(value) = manifest.latest_results.get(&work.id) {
                                    head = Some(value.as_ref().map(|value| WorkCompletionRef {
                                        brief_id: value.brief_id.clone(),
                                        transcript_entry_id: value.transcript_entry_id.clone(),
                                        source_message_id: value.source_message_id.clone(),
                                    }));
                                }
                            }
                        }
                        coverage.latest_results.insert(
                            work.id.clone(),
                            head.context("Wait cleanup owner head is unprepared")?,
                        );
                    }
                }
                CanonicalWorkRecord::Wait(wait) => {
                    ensure!(
                        coverage.unresolved_wait_ids.contains(&wait.id)
                            == matches!(
                                wait.status,
                                WaitConditionStatus::Active | WaitConditionStatus::Triggered
                            ),
                        "Wait cleanup cannot invent complete unresolved Wait membership"
                    );
                    relations::register_wait_edges_tx(tx, &proof.scope, wait, version, false)?;
                }
                CanonicalWorkRecord::Frame(_) => {
                    anyhow::bail!("Wait owning closure unexpectedly contains a frame")
                }
            }
            coverage
                .observations
                .insert(child.clone(), WorkAvailability::Present);
            pending.extend(current.record.references());
            records.insert(child, current.qualified(&proof.scope, version)?);
        }
    }
    for record in records.values() {
        if let CanonicalWorkRecord::Work(work) = &record.record {
            let head = coverage
                .latest_results
                .get(&work.id)
                .and_then(Option::as_ref)
                .map(|head| head.brief_id.as_str());
            if let Some(id) = &work.result_brief_id {
                if head != Some(id.as_str()) {
                    owning_roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Brief, id));
                }
            }
            if let Some(intent) = &work.completion_intent {
                if let Some(id) = &intent.result_brief_id {
                    if head != Some(id.as_str()) {
                        owning_roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Brief, id));
                    }
                }
                if let Some(id) = &intent.source_message_id {
                    owning_roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Message, id));
                }
                if let Some(id) = &intent.source_turn_id {
                    owning_roots.insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, id));
                }
            }
        }
    }
    let changed = !records.is_empty();
    coverage.revision = if changed {
        revision
    } else {
        proof.base_revision
    };
    loader::persist_coverage(tx, &proof.scope, &coverage)?;
    Ok(Some(CommittedWorkFacts {
        delta: CommittedWorkDelta {
            scope: proof.scope.clone(),
            base_revision: proof.base_revision,
            revision: coverage.revision,
            records: records.into_values().collect(),
            removals: BTreeMap::new(),
            coverage,
        },
        owning_input_roots: owning_roots,
    }))
}

fn register_state_less_waits(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    actual: &BTreeSet<WorkKey>,
    request: &WorkDomainLoadRequest,
) -> Result<()> {
    let mut bytes = 0usize;
    for key in actual {
        let before = proof
            .before
            .get(key)
            .and_then(Option::as_ref)
            .context("State-less startup Wait has no original qualification")?;
        let after = stored_on(tx, &proof.scope.agent_id, key)?
            .context("State-less startup Wait disappeared")?;
        let prior = alias(tx, &proof.scope, key)?;
        ensure!(
            !prior
                .as_ref()
                .is_some_and(|a| a.availability == WorkAvailability::Pruned),
            "State-less startup cannot resurrect a pruned Wait"
        );
        // There is no published coverage/root to advance. Preserve its first
        // materialized version; a future bootstrap prepares complete coverage.
        ensure!(
            prior.as_ref().is_none_or(|a| a.revision == 1),
            "State-less startup has an alias newer than missing coverage"
        );
        bytes = bytes.saturating_add(after.payload.len());
        ensure!(
            bytes <= request.byte_limit && actual.len() <= request.record_limit,
            "State-less startup Wait ACK budget exhausted"
        );
        if before.payload != after.payload || before.fingerprint != after.fingerprint {
            upsert_alias(
                tx,
                &proof.scope,
                key,
                &Alias {
                    revision: 1,
                    availability: WorkAvailability::Present,
                    digest: Some(after.digest()),
                    fingerprint: Some(after.fingerprint.clone()),
                },
            )?;
        }
        let CanonicalWorkRecord::Wait(wait) = &after.record else {
            unreachable!()
        };
        relations::register_wait_edges_tx(tx, &proof.scope, wait, 1, false)?;
    }
    Ok(())
}

fn update_wait_membership(coverage: &mut WorkCoverage, wait: &WaitConditionRecord) {
    coverage.unresolved_wait_ids.remove(&wait.id);
    for (source, ids) in &mut coverage.turn_wait_keys {
        ids.remove(&wait.id);
        if wait.turn_id.as_ref() == Some(source) {
            ids.insert(wait.id.clone());
        }
    }
    for (source, ids) in &mut coverage.task_wait_keys {
        ids.remove(&wait.id);
        if wait
            .wake_sources
            .iter()
            .any(|wake| matches!(wake,WakeSource::TaskResult{task_id} if task_id==source))
        {
            ids.insert(wait.id.clone());
        }
    }
    for (source, ids) in &mut coverage.trigger_wait_keys {
        ids.remove(&wait.id);
        if wait.trigger_message_id().is_some_and(|id| id == source) {
            ids.insert(wait.id.clone());
        }
    }
}

/// The original startup/Control actor may still clean unqualified historical rows, but
/// only fixed/current members receive a qualification version update.
pub(crate) struct PreparedExistingWorkWaitCleanup {
    proof: WorkWriteProof,
    candidates: BTreeSet<String>,
    qualified_waits: BTreeSet<String>,
}
pub(crate) fn prepare_existing_work_wait_cleanup_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    wait_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
    original_recovery: bool,
) -> Result<PreparedExistingWorkWaitCleanup> {
    prepare_existing(tx, scope, wait_ids, request, original_recovery)
}
fn prepare_existing(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    wait_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
    original: bool,
) -> Result<PreparedExistingWorkWaitCleanup> {
    ensure!(
        scope_facts(tx, scope, original)?.is_some(),
        "startup Work identity changed"
    );
    let seed = binding(tx, scope)?;
    qualification::manifests(tx, seed.as_deref())?;
    bound_metadata(wait_ids.len(), wait_ids.iter().map(String::len).sum())?;
    let mut qualified_waits = BTreeSet::new();
    for id in wait_ids {
        let key = WorkKey::new(WorkFamily::Wait, id);
        let prior = alias(tx, scope, &key)?;
        ensure!(
            !prior
                .as_ref()
                .is_some_and(|a| a.availability == WorkAvailability::Pruned),
            "startup Wait cannot reuse a pruned ID"
        );
        if prior.is_some_and(|a| a.availability == WorkAvailability::Present)
            || qualification::eligible_digest(tx, seed.as_deref(), &key)?.is_some()
        {
            qualified_waits.insert(id.clone());
        }
    }
    let proof = if original {
        prepare_original_work_wait_cleanup_tx(tx, scope, &qualified_waits, request)?
    } else {
        prepare_work_wait_cleanup_tx(tx, scope, &qualified_waits, request)?
    };
    Ok(PreparedExistingWorkWaitCleanup {
        proof,
        candidates: wait_ids.clone(),
        qualified_waits,
    })
}
pub(crate) fn register_existing_work_wait_cleanup_tx(
    tx: &Transaction<'_>,
    proof: &PreparedExistingWorkWaitCleanup,
    writes: &repositories::WaitConditionWrites,
    final_state: Option<&AgentState>,
    request: &WorkDomainLoadRequest,
) -> Result<Option<CommittedWorkFacts>> {
    ensure!(
        writes.wait_ids.is_subset(&proof.candidates),
        "startup has unprepared actual Wait writes"
    );
    let mut wait_ids = writes
        .wait_ids
        .intersection(&proof.qualified_waits)
        .peekable();
    if wait_ids.peek().is_none() {
        return Ok(None);
    }
    register_wait_cleanup_ids_tx(tx, &proof.proof, wait_ids, final_state, request)
}
