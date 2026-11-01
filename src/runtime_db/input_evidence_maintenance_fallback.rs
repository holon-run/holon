//! The retired scheduler fallback has its own terminal semantics. Its bounded
//! provenance is tied to this original transaction, not an Interrupt receipt.
use super::*;
use crate::domain::execution_protocol::{
    ExecutionAttempt, ExecutionAttemptState, ExecutionProtocolState, WorkItemExecutionState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FallbackOrigin {
    seed_id: String,
    boundary_id: String,
    source_kind: String,
    fixed_roots: BTreeSet<Key>,
    key: Key,
    recovery_message_id: String,
    predecessor: Option<ExecutionAttempt>,
}

pub(crate) struct PreparedFallbackInputs {
    original: PreparedOfflineInputRepair,
    active: Option<InputEvidenceScope>,
    recovery_message: Key,
    outcomes: BTreeMap<Key, ExecutionAttempt>,
    fixed_roots: BTreeSet<Key>,
}

pub(crate) fn prepare_tx(
    tx: &Transaction<'_>,
    db: &RuntimeDb,
    agent: &str,
    state: &ExecutionProtocolState,
    dequeued_messages: &[String],
    recovery_message_id: &str,
    authority: Option<&OfflineInputMaintenance<'_>>,
) -> Result<PreparedFallbackInputs> {
    let mut keys = BTreeSet::new();
    let mut outcomes = BTreeMap::new();
    for attempt in state
        .attempts
        .values()
        .filter(|a| a.state == ExecutionAttemptState::Open)
    {
        keys.insert(Key::new(Family::Attempt, &attempt.attempt_id));
        if let Some(source) = &attempt.source_message_id {
            keys.insert(Key::new(Family::Message, source));
        }
        outcomes.insert(
            Key::new(
                Family::Outcome,
                format!("retired-scheduler-cleanup:{}", attempt.attempt_id),
            ),
            attempt.clone(),
        );
    }
    for (id, _) in state
        .work_items
        .iter()
        .filter(|(_, w)| matches!(w.state, WorkItemExecutionState::InFlight { .. }))
    {
        keys.insert(Key::new(Family::WorkExecution, id));
    }
    keys.extend(
        dequeued_messages
            .iter()
            .map(|id| Key::new(Family::Message, id)),
    );
    let mut original = prepare_offline_input_repair_tx(
        tx,
        db,
        agent,
        &keys.into_iter().collect::<Vec<_>>(),
        &[],
        authority,
    )?;
    outcomes.retain(|_, attempt| {
        original
            .allowed
            .contains(&Key::new(Family::Attempt, &attempt.attempt_id))
            && attempt
                .source_message_id
                .as_ref()
                .is_some_and(|id| original.allowed.contains(&Key::new(Family::Message, id)))
    });
    let recovery_message = Key::new(Family::Message, recovery_message_id);
    let enabled: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='input_evidence_current')", [], |r| r.get(0))?;
    let active = if authority.is_some() && enabled {
        let identity: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
                [agent],
                |r| r.get(0),
            )
            .optional()?;
        identity
            .map(|json| serde_json::from_str::<AgentIdentityRecord>(&json))
            .transpose()?
            .filter(|i| i.status == AgentRegistryStatus::Active)
            .map(|i| InputEvidenceScope {
                agent_id: agent.into(),
                identity_incarnation: i.incarnation,
            })
    } else {
        None
    };
    let mut fixed_roots = BTreeSet::new();
    match &mut original.role {
        RepairRole::Current(scope, proof) => {
            let mut derived = outcomes.keys().cloned().collect::<BTreeSet<_>>();
            // Deleting may only derive a recovery event from its original roots.
            if active.is_none() && !original.allowed.is_empty() {
                derived.insert(recovery_message.clone());
            }
            *proof = ExistingAdmissionInputProof::prepare_tx(
                tx,
                scope,
                original.allowed.clone(),
                derived.clone(),
            )?;
            original.allowed.extend(derived);
        }
        RepairRole::Unbound(prepared) => {
            for key in &original.allowed {
                flatten_fixed_roots_tx(tx, &prepared.scope.seed_id, key, &mut fixed_roots)?;
            }
            ensure!(
                original
                    .allowed
                    .len()
                    .saturating_add(outcomes.len())
                    .saturating_add(1)
                    <= MAX_RECORDS,
                "fallback qualification footprint exceeds input record limit"
            );
            let origins = outcomes
                .iter()
                .map(|(key, before)| (key.clone(), Some(before.clone())))
                .chain(std::iter::once((recovery_message.clone(), None)));
            let mut origin_bytes = 0usize;
            for (key, predecessor) in origins {
                let origin = FallbackOrigin {
                    seed_id: prepared.scope.seed_id.clone(),
                    boundary_id: prepared.scope.boundary_id.clone(),
                    source_kind: prepared.scope.source_kind.clone(),
                    fixed_roots: fixed_roots.clone(),
                    key,
                    recovery_message_id: recovery_message_id.into(),
                    predecessor,
                };
                origin_bytes = origin_bytes
                    .checked_add(serde_json::to_vec(&origin)?.len())
                    .context("fallback origin size overflow")?;
                ensure!(
                    origin_bytes <= MAX_METADATA_BYTES,
                    "fallback provenance exceeds input metadata limit"
                );
            }
            let mut bounded = BTreeSet::new();
            let mut bytes = 0;
            extend_metadata_selection(&mut bounded, &mut bytes, fixed_roots.iter().cloned())?;
        }
        RepairRole::Unqualified => {
            outcomes.clear();
        }
    }
    Ok(PreparedFallbackInputs {
        original,
        active,
        recovery_message,
        outcomes,
        fixed_roots,
    })
}

impl PreparedFallbackInputs {
    /// `inserted` is the actual append result from this original transaction.
    pub(crate) fn finish_tx(
        self,
        tx: &Transaction<'_>,
        message: &MessageEnvelope,
        inserted: bool,
    ) -> Result<()> {
        ensure!(
            inserted
                && message.id == self.recovery_message.id
                && message.agent_id == self.original.agent,
            "fallback recovery message was not newly inserted in its original transaction"
        );
        for (key, before) in &self.outcomes {
            validate_terminal_tx(tx, &self.original.agent, key, before)?;
        }
        match self.original.role {
            RepairRole::Current(scope, proof) => {
                let records = exact_records_tx(tx, &self.original.agent, &self.original.allowed)?;
                register_committed_cleanup_input_evidence_tx(tx, &scope, &records, &proof)?;
            }
            RepairRole::Unbound(prepared) => {
                let records = exact_records_tx(tx, &self.original.agent, &self.original.allowed)?;
                legacy_recovery::register_unbound_seed_recovery_tx(tx, &prepared, &records, &[])?;
                let mut created = self.outcomes.keys().cloned().collect::<BTreeSet<_>>();
                if !self.fixed_roots.is_empty() {
                    created.insert(self.recovery_message.clone());
                }
                for key in created {
                    let origin = FallbackOrigin {
                        seed_id: prepared.scope.seed_id.clone(),
                        boundary_id: prepared.scope.boundary_id.clone(),
                        source_kind: prepared.scope.source_kind.clone(),
                        fixed_roots: self.fixed_roots.clone(),
                        predecessor: self.outcomes.get(&key).cloned(),
                        key: key.clone(),
                        recovery_message_id: message.id.clone(),
                    };
                    write_origin_tx(tx, &prepared.scope.agent_id, &origin)?;
                }
            }
            RepairRole::Unqualified => {}
        }
        if let Some(scope) = self.active {
            register_committed_input_evidence_tx(
                tx,
                &scope,
                &[CanonicalInputEvidence::Message(message.clone())],
            )?;
        }
        Ok(())
    }
}
fn exact_records_tx(
    tx: &Transaction<'_>,
    agent: &str,
    keys: &BTreeSet<Key>,
) -> Result<Vec<CanonicalInputEvidence>> {
    keys.iter()
        .map(|key| {
            decode_canonical(
                key,
                &payload_on(tx, agent, key)?.context("fallback actual row missing")?,
            )
        })
        .collect()
}
fn flatten_fixed_roots_tx(
    tx: &Transaction<'_>,
    seed: &str,
    key: &Key,
    roots: &mut BTreeSet<Key>,
) -> Result<()> {
    let fixed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE seed_id=?1 AND family=?2 AND record_id=?3)",params![seed,key.family.as_str(),key.id],|r|r.get(0))?;
    if fixed {
        roots.insert(key.clone());
        return Ok(());
    }
    legacy_recovery::validate_seed_overlay_on(tx, seed, key)?;
    let (kind,attempt,json):(String,Option<String>,Option<String>)=tx.query_row("SELECT origin_kind,origin_attempt_id,maintenance_proof_json FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![seed,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if kind == "recovery_created" {
        roots.insert(Key::new(
            Family::Attempt,
            attempt.context("fallback predecessor missing")?,
        ));
    } else {
        let origin: FallbackOrigin =
            serde_json::from_str(&json.context("fallback origin missing")?)?;
        roots.extend(origin.fixed_roots);
    }
    Ok(())
}
fn validate_terminal_tx(
    tx: &Transaction<'_>,
    agent: &str,
    key: &Key,
    before: &ExecutionAttempt,
) -> Result<()> {
    let payload = payload_on(tx, agent, &Key::new(Family::Attempt, &before.attempt_id))?
        .context("fallback predecessor missing")?;
    let mut expected = before.clone();
    let actual: ExecutionAttempt = serde_json::from_str(&payload)?;
    ensure!(
        before.state == ExecutionAttemptState::Open
            && actual.state == ExecutionAttemptState::Interrupted
            && actual.terminal_outcome_id.as_deref() == Some(key.id.as_str())
            && actual.terminal_at.is_some(),
        "fallback predecessor did not make its exact terminal transition"
    );
    expected.state = actual.state.clone();
    expected.terminal_outcome_id = actual.terminal_outcome_id.clone();
    expected.terminal_at = actual.terminal_at.clone();
    ensure!(
        expected == actual,
        "fallback changed predecessor identity/source/fences"
    );
    validate_created_outcome_tx(tx, agent, key, before)
}
fn validate_created_outcome_tx(
    tx: &Transaction<'_>,
    agent: &str,
    key: &Key,
    before: &ExecutionAttempt,
) -> Result<()> {
    let record = decode_canonical(
        key,
        &payload_on(tx, agent, key)?.context("fallback Outcome missing")?,
    )?;
    let CanonicalInputEvidence::Outcome(outcome) = record else {
        unreachable!()
    };
    let expected =
        crate::runtime_db::retired_scheduler_cleanup::fallback_outcome(before, &outcome.created_at);
    ensure!(
        outcome == expected,
        "fallback Outcome differs from original terminal semantics"
    );
    Ok(())
}
fn write_origin_tx(tx: &Transaction<'_>, agent: &str, origin: &FallbackOrigin) -> Result<()> {
    ensure!(
        !origin.fixed_roots.is_empty(),
        "fallback has no fixed provenance"
    );
    let mut fingerprint =
        fingerprint_on(tx, agent, &origin.key)?.context("fallback created metadata missing")?;
    if requires_frozen_body_digest(&origin.key, &fingerprint) {
        fingerprint.frozen_payload_digest =
            Some(bounded_body_digest(tx, agent, &origin.key, &fingerprint)?);
    }
    let json = serde_json::to_string(origin)?;
    ensure!(
        json.len() <= MAX_METADATA_BYTES,
        "fallback origin exceeds input metadata budget"
    );
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3) OR EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE seed_id=?1 AND family=?2 AND record_id=?3)",params![origin.seed_id,origin.key.family.as_str(),origin.key.id],|r|r.get(0))?;
    ensure!(!exists, "fallback derived ID already has a legacy origin");
    tx.execute("INSERT INTO input_evidence_seed_recovery_current(seed_id,family,record_id,revision,fingerprint_json,origin_kind,maintenance_proof_json) VALUES(?1,?2,?3,1,?4,'maintenance_created',?5)",params![origin.seed_id,origin.key.family.as_str(),origin.key.id,serde_json::to_string(&fingerprint)?,json])?;
    validate_origin_tx(tx, &origin.seed_id, &origin.key)?;
    if origin.key.family == Family::Message {
        execution::register_maintenance_created_source_tx(
            tx,
            &origin.seed_id,
            agent,
            &origin.key.id,
        )?;
    }
    Ok(())
}
pub(crate) fn validate_origin_tx(tx: &Transaction<'_>, seed: &str, key: &Key) -> Result<()> {
    let json:String=tx.query_row("SELECT maintenance_proof_json FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![seed,key.family.as_str(),key.id],|r|r.get(0))?;
    ensure!(
        json.len() <= MAX_METADATA_BYTES,
        "fallback origin exceeds input metadata budget"
    );
    let origin: FallbackOrigin = serde_json::from_str(&json)?;
    ensure!(
        origin.seed_id == seed && origin.key == *key && !origin.fixed_roots.is_empty(),
        "fallback origin namespace differs"
    );
    let (agent, boundary, source): (String, String, String) = tx.query_row(
        "SELECT agent_id,boundary_id,source_kind FROM episode_continuity_seeds WHERE seed_id=?1",
        [seed],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        boundary == origin.boundary_id && source == origin.source_kind,
        "fallback fixed seed changed"
    );
    let mut bounded = BTreeSet::new();
    let mut bytes = 0;
    extend_metadata_selection(&mut bounded, &mut bytes, origin.fixed_roots.iter().cloned())?;
    for root in &origin.fixed_roots {
        let fixed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE seed_id=?1 AND family=?2 AND record_id=?3)",params![seed,root.family.as_str(),root.id],|r|r.get(0))?;
        ensure!(
            fixed,
            "fallback provenance is not immutable fixed membership"
        );
    }
    match key.family {
        Family::Outcome => {
            let before = origin
                .predecessor
                .context("fallback Outcome predecessor missing")?;
            ensure!(
                origin
                    .fixed_roots
                    .contains(&Key::new(Family::Attempt, &before.attempt_id))
                    && before.source_message_id.as_ref().is_some_and(|id| origin
                        .fixed_roots
                        .contains(&Key::new(Family::Message, id))),
                "fallback Outcome lacks fixed admission/source"
            );
            validate_created_outcome_tx(tx, &agent, key, &before)?;
        }
        Family::Message => {
            ensure!(
                origin.predecessor.is_none() && key.id == origin.recovery_message_id,
                "fallback recovery Message origin differs"
            );
        }
        _ => anyhow::bail!("unsupported fallback derived family"),
    }
    Ok(())
}

#[cfg(test)]
#[path = "input_evidence_maintenance_fallback_tests.rs"]
mod tests;
