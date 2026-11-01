//! Same-transaction observations of an Attempt's actual canonical Turn closure.
//! A Message/Attempt turn_id alone remains only a preallocated/replay hint.
use super::*;
use crate::domain::execution_protocol::{
    AdmittedFences, ExecutionAttemptState, ExecutionBinding, ExecutionSource,
};
use crate::types::{
    AgentMessageDeliveryState, AgentState, AgentStatus, QueueEntryRecord, QueueEntryStatus,
};

fn isolated_delivery_hint_tx(
    tx: &Transaction<'_>,
    attempt: &ExecutionAttempt,
    message: &MessageEnvelope,
) -> Result<bool> {
    if attempt.turn_id.is_some()
        || !matches!(&attempt.binding, ExecutionBinding::AgentLifecycle { agent_id }
            if agent_id == &attempt.agent_id)
    {
        return Ok(false);
    }
    let Some(delivery) =
        crate::runtime_db::agent_message_delivery::delivery_by_message_id_tx(tx, &message.id)?
    else {
        return Ok(false);
    };
    // Claim already bound and dispatched this exact delivery in the original
    // transaction. Its foreign caller's Turn hint is not a target assignment.
    Ok(delivery.state == AgentMessageDeliveryState::Dispatched
        && delivery.activation_id.as_deref() == Some(attempt.attempt_id.as_str())
        && delivery.turn_id.is_none()
        && crate::runtime::legacy_cross_agent_caller_bindings_match(message, &delivery))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AttemptTurnProof {
    source_message_id: Option<String>,
    source: ExecutionSource,
    binding: ExecutionBinding,
    fences: AdmittedFences,
    admitted_at: String,
    hint_turn_id: Option<String>,
    /// Empty is a verified claim-only observation, not missing metadata.
    pub(super) turn_keys: BTreeSet<Key>,
}
impl AttemptTurnProof {
    pub(super) fn for_output(attempt: &ExecutionAttempt, turn_keys: BTreeSet<Key>) -> Self {
        Self {
            source_message_id: attempt.source_message_id.clone(),
            source: attempt.source.clone(),
            binding: attempt.binding.clone(),
            fences: attempt.admitted_fences.clone(),
            admitted_at: attempt.admitted_at.clone(),
            hint_turn_id: attempt.turn_id.clone(),
            turn_keys,
        }
    }
    fn matches_attempt(&self, attempt: &ExecutionAttempt) -> bool {
        self.source_message_id == attempt.source_message_id
            && self.source == attempt.source
            && self.binding == attempt.binding
            && self.fences == attempt.admitted_fences
            && self.admitted_at == attempt.admitted_at
    }
}

fn canonical_turn(tx: &Transaction<'_>, agent: &str, turn_id: &str) -> Result<bool> {
    let record: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id=?1",
            [turn_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(payload) = record else {
        return Ok(false);
    };
    let record: TurnRecord = serde_json::from_str(&payload)?;
    ensure!(
        record.agent_id == agent && record.turn_id == turn_id,
        "Attempt Turn has a different canonical scope"
    );
    Ok(true)
}
fn state_on(tx: &Transaction<'_>, agent: &str) -> Result<Option<AgentState>> {
    tx.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [agent],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|p| serde_json::from_str(&p).map_err(anyhow::Error::from))
    .transpose()
}
fn source_assignment(tx: &Transaction<'_>, attempt: &ExecutionAttempt) -> Result<Option<String>> {
    let Some(message) = &attempt.source_message_id else {
        return Ok(None);
    };
    let assignment: Option<(String, String)> = tx
        .query_row(
            "SELECT agent_id,turn_id FROM conversation_input_assignments WHERE message_id=?1",
            [message],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    assignment
        .map(|(agent, turn)| {
            ensure!(
                agent == attempt.agent_id,
                "Attempt source assignment belongs to another agent"
            );
            Ok(turn)
        })
        .transpose()
}
fn strict_unstarted(
    tx: &Transaction<'_>,
    attempt: &ExecutionAttempt,
    state: Option<&AgentState>,
) -> Result<bool> {
    let Some(state) = state else { return Ok(false) };
    let Some(source) = attempt.source_message_id.as_deref() else {
        return Ok(false);
    };
    if attempt.state != ExecutionAttemptState::Open
        || state.status != AgentStatus::AwakeRunning
        || state.current_run_id.is_none()
        || state.current_execution_binding.as_ref().is_some_and(|b| {
            b.activation_id.as_deref() == Some(attempt.attempt_id.as_str())
                || attempt.turn_id.as_deref() == Some(b.turn_id.as_str())
        })
        || attempt
            .turn_id
            .as_ref()
            .is_some_and(|id| state.current_turn_id.as_ref() == Some(id))
        || source_assignment(tx, attempt)?.is_some()
    {
        return Ok(false);
    }
    let message: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT payload_json,message_seq FROM messages WHERE message_id=?1 AND agent_id=?2",
            params![source, attempt.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((payload, seq)) = message else {
        return Ok(false);
    };
    let message: MessageEnvelope = serde_json::from_str(&payload)?;
    if message.agent_id != attempt.agent_id
        || message.id != source
        || message.message_seq.and_then(|s| i64::try_from(s).ok()) != seq
        || message.message_seq != Some(attempt.source.generation)
        || message.message_seq != Some(attempt.admitted_fences.source_revision)
        || (message.turn_id != attempt.turn_id
            && !isolated_delivery_hint_tx(tx, attempt, &message)?)
    {
        return Ok(false);
    }
    let queue: Option<(String, String, String)> = tx
        .query_row(
            "SELECT agent_id,status,payload_json FROM queue_entries WHERE message_id=?1",
            [source],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((agent, status, queue)) = queue else {
        return Ok(false);
    };
    let queue: QueueEntryRecord = serde_json::from_str(&queue)?;
    ensure!(
        agent == queue.agent_id
            && serde_json::to_value(&queue.status)?.as_str() == Some(status.as_str()),
        "claim queue normalized fields differ from payload"
    );
    Ok(queue.agent_id == attempt.agent_id
        && queue.message_id == source
        && matches!(
            queue.status,
            QueueEntryStatus::Dequeued | QueueEntryStatus::Interjected
        ))
}

/// Only an exact recovery successor can disambiguate a message-wide assignment.
/// Never observe it recursively: that would prove it from this same edge.
fn assigned_to_successor(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    attempt: &ExecutionAttempt,
    state: Option<&AgentState>,
    turn: &str,
) -> Result<bool> {
    ensure!(
        scope.agent_id == attempt.agent_id,
        "Attempt proof scope changed"
    );
    // The caller already loaded State; only identity is needed for this fence.
    let identity = ContextDurableRead::new(tx, &scope.agent_id).identity_fact()?;
    if !identity.is_some_and(|identity| {
        identity.incarnation == scope.identity_incarnation
            && identity.status == AgentRegistryStatus::Active
    }) {
        return Err(InputEvidenceSnapshotError::RetiredScope.into());
    }
    let ids = {
        let mut stmt = tx.prepare(
            "SELECT attempt_id FROM execution_protocol_attempts WHERE agent_id=?1 AND recovery_of_attempt_id=?2 LIMIT 2",
        )?;
        let rows = stmt.query_map(params![attempt.agent_id, attempt.attempt_id], |r| {
            r.get::<_, String>(0)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let [id] = ids.as_slice() else {
        return Ok(false);
    };
    let key = Key::new(Family::Attempt, id);
    let payload = payload_on(tx, &attempt.agent_id, &key)?.context("recovery successor missing")?;
    let successor: ExecutionAttempt = serde_json::from_str(&payload)?;
    ensure!(
        successor.attempt_id == *id && successor.agent_id == scope.agent_id,
        "recovery successor canonical scope changed"
    );
    if successor.attempt_id == attempt.attempt_id
        || successor.recovery_of_attempt_id.as_deref() != Some(attempt.attempt_id.as_str())
        || successor.source != attempt.source
        || successor.source_message_id != attempt.source_message_id
        || successor.binding != attempt.binding
        || successor.provenance != attempt.provenance
        || successor.admitted_fences.source_revision != successor.source.generation
    {
        return Ok(false);
    }
    let binding = binding_on(tx, scope)?;
    let proof = fixed_proof_on(tx, scope, &binding, &successor)?;
    if let Some(binding) = state.and_then(|s| s.current_execution_binding.as_ref()) {
        if binding.turn_id == turn {
            return Ok(binding.activation_id.as_deref() == Some(id.as_str())
                && successor.source_message_id.as_deref()
                    == Some(binding.source_message_id.as_str()));
        }
    }
    Ok(proof.is_some_and(|p| p.turn_keys.contains(&Key::new(Family::Turn, turn))))
}

/// Prior None is usable only from the original exact namespace and source/fences.
/// Any new actual Turn relation wins; a prior Some can never degrade to None.
pub(super) fn observe(
    tx: &Transaction<'_>,
    scope: Option<&InputEvidenceScope>,
    attempt: &ExecutionAttempt,
    prior: Option<&AttemptTurnProof>,
) -> Result<AttemptTurnProof> {
    if let Some(prior) = prior {
        ensure!(
            prior.matches_attempt(attempt),
            "Attempt Turn proof source/fences changed"
        );
    }
    if prior.is_some_and(|p| p.hint_turn_id != attempt.turn_id) {
        ensure!(
            attempt
                .turn_id
                .as_deref()
                .map(|id| canonical_turn(tx, &attempt.agent_id, id))
                .transpose()?
                .unwrap_or(false),
            "Attempt changed its hint without a canonical Turn"
        );
    }
    let state = state_on(tx, &attempt.agent_id)?;
    let mut turns = BTreeSet::new();
    if let Some(turn) = attempt.turn_id.as_deref() {
        if canonical_turn(tx, &attempt.agent_id, turn)? {
            turns.insert(Key::new(Family::Turn, turn));
        }
    }
    if let Some(binding) = state
        .as_ref()
        .and_then(|s| s.current_execution_binding.as_ref())
    {
        if binding.activation_id.as_deref() == Some(attempt.attempt_id.as_str())
            || (attempt.source_message_id.as_deref() == Some(binding.source_message_id.as_str())
                && attempt.turn_id.as_deref() == Some(binding.turn_id.as_str()))
        {
            turns.insert(Key::new(Family::Turn, &binding.turn_id));
        }
    }
    if let Some(turn) = source_assignment(tx, attempt)? {
        let key = Key::new(Family::Turn, &turn);
        // Excluding an edge must not hide a missing or foreign canonical Turn.
        if !canonical_turn(tx, &attempt.agent_id, &turn)?
            && !scope
                .map(|s| alias_on(tx, s, &key))
                .transpose()?
                .flatten()
                .is_some_and(|a| !a.present)
        {
            return Err(InputEvidenceSnapshotError::NeedsPreparation(format!(
                "started/assigned Attempt Turn {turn} is missing"
            ))
            .into());
        }
        let successor_owned = if turns.is_empty()
            && attempt.state == ExecutionAttemptState::Interrupted
            && attempt.run_id.is_none()
            && prior.is_some_and(|p| p.turn_keys.is_empty())
        {
            match scope {
                Some(scope) => assigned_to_successor(tx, scope, attempt, state.as_ref(), &turn)?,
                None => false,
            }
        } else {
            false
        };
        if !successor_owned {
            turns.insert(key);
        }
    }
    if let Some(prior) = prior {
        turns.extend(prior.turn_keys.iter().cloned());
    }
    for turn in &turns {
        if !canonical_turn(tx, &attempt.agent_id, &turn.id)? {
            let pruned = scope
                .map(|s| alias_on(tx, s, turn))
                .transpose()?
                .flatten()
                .is_some_and(|a| !a.present);
            if !pruned {
                return Err(InputEvidenceSnapshotError::NeedsPreparation(format!(
                    "started/assigned Attempt Turn {} is missing",
                    turn.id
                ))
                .into());
            }
        }
    }
    if turns.is_empty() && prior.is_none() && !strict_unstarted(tx, attempt, state.as_ref())? {
        return Err(InputEvidenceSnapshotError::NeedsPreparation(format!(
            "Attempt {} lacks fixed claim-only Turn proof",
            attempt.attempt_id
        ))
        .into());
    }
    Ok(AttemptTurnProof {
        source_message_id: attempt.source_message_id.clone(),
        source: attempt.source.clone(),
        binding: attempt.binding.clone(),
        fences: attempt.admitted_fences.clone(),
        admitted_at: attempt.admitted_at.clone(),
        hint_turn_id: attempt.turn_id.clone(),
        turn_keys: turns,
    })
}

pub(super) fn for_current(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    attempt: &ExecutionAttempt,
) -> Result<AttemptTurnProof> {
    let prior = fixed_proof_on(tx, scope, binding, attempt)?;
    let observed = observe(tx, Some(scope), attempt, prior.as_ref())?;
    if prior.is_none() && observed.turn_keys.is_empty() {
        let source = attempt
            .source_message_id
            .as_ref()
            .context("unstarted Attempt has no exact source")?;
        ensure!(
            is_existing_member_on(tx, scope, binding, &Key::new(Family::Message, source))?,
            "unstarted Attempt source is not qualified in this incarnation"
        );
    }
    Ok(observed)
}

fn fixed_proof_on(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    attempt: &ExecutionAttempt,
) -> Result<Option<AttemptTurnProof>> {
    let key = Key::new(Family::Attempt, &attempt.attempt_id);
    let old = alias_on(tx, scope, &key)?;
    let mut prior = old
        .as_ref()
        .and_then(|a| a.attempt_turn_proof.as_ref())
        .cloned();
    if prior.is_none() {
        if let Some(seed) = binding.seed_id.as_deref() {
            let proofs = {
                let mut stmt=tx.prepare("SELECT proof_json FROM input_evidence_seed_attempt_turns WHERE seed_id=?1 AND attempt_id=?2 ORDER BY capture_phase")?;
                let rows =
                    stmt.query_map(params![seed, attempt.attempt_id], |r| r.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for proof in proofs {
                let p: AttemptTurnProof = serde_json::from_str(&proof)?;
                ensure!(
                    p.matches_attempt(attempt),
                    "fixed Attempt Turn proof source/fences changed"
                );
                if let Some(old) = prior.as_mut() {
                    old.turn_keys.extend(p.turn_keys);
                } else {
                    prior = Some(p);
                }
            }
        }
    }
    if let Some(prior) = &prior {
        ensure!(
            prior.matches_attempt(attempt),
            "Attempt Turn proof source/fences changed"
        );
    }
    Ok(prior)
}
pub(super) fn capture(
    tx: &Transaction<'_>,
    seed: &str,
    phase: &str,
    attempt: &ExecutionAttempt,
) -> Result<AttemptTurnProof> {
    let proof = observe(tx, None, attempt, None)?;
    let json = serde_json::to_string(&proof)?;
    tx.execute("INSERT INTO input_evidence_seed_attempt_turns(seed_id,capture_phase,attempt_id,proof_json) VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",params![seed,phase,attempt.attempt_id,json])?;
    let actual:String=tx.query_row("SELECT proof_json FROM input_evidence_seed_attempt_turns WHERE seed_id=?1 AND capture_phase=?2 AND attempt_id=?3",params![seed,phase,attempt.attempt_id],|r|r.get(0))?;
    ensure!(actual == json, "fixed Attempt Turn proof changed");
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domain::execution_protocol::*, runtime_db::RuntimeDb, types::*};

    #[test]
    fn input_claim_only_isolation_requires_the_exact_dispatched_delivery() -> Result<()> {
        for variant in [
            "exact",
            "missing",
            "activation",
            "queued",
            "caller",
            "bindings",
        ] {
            let dir = tempfile::tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                dir.path().join("runtime.sqlite"),
                dir.path().join("runtime.lock"),
            )?;
            db.transaction(|tx| {
                let caller = AgentMessageCallerContext {
                    caller_principal: "runtime:agent-invocation".into(),
                    caller_agent_id: Some("caller".into()),
                    principal_kind: AgentMessagePrincipalKind::RuntimeCapability,
                    route: "agent_invocation".into(),
                    origin: MessageOrigin::Task { task_id: "caller-task".into() },
                    authority_class: AuthorityClass::RuntimeInstruction,
                    delivery_surface: MessageDeliverySurface::RuntimeSystem,
                    admission_context: AdmissionContext::RuntimeOwned,
                    current_turn_id: Some("caller-turn".into()),
                    current_task_id: Some("caller-task".into()),
                    current_work_item_id: Some("caller-work".into()),
                    inherited_routing_context: None, routing_activation_id: None,
                };
                let mut prepared = crate::runtime::AgentMessageDeliveryService::prepare(
                    AgentMessageSendRequest {
                        target_agent_id: "target".into(),
                        content: MessageBody::Text { text: "legacy invocation".into() },
                        client_idempotency_key: "legacy".into(), correlation_id: None,
                        causation_id: None, requested_priority: None, forward: false, reply_context_id: None,
                    }, caller.clone(),
                )?;
                prepared.message.turn_id.clone_from(&caller.current_turn_id);
                prepared.message.task_id.clone_from(&caller.current_task_id);
                prepared.message.work_item_id.clone_from(&caller.current_work_item_id);
                let message = crate::runtime_db::evidence::append_message_tx(tx, &prepared.message)?.0;
                let mut state = AgentState::new("target");
                state.status = AgentStatus::AwakeRunning;
                state.current_run_id = Some("original-claim-run".into());
                crate::runtime_db::evidence::upsert_agent_state_tx(tx, &state)?;
                crate::runtime_db::repositories::upsert_queue_entry_tx(tx, &QueueEntryRecord {
                    message_id: message.id.clone(), agent_id: "target".into(),
                    priority: message.priority.clone(), status: QueueEntryStatus::Dequeued,
                    created_at: message.created_at, updated_at: message.created_at,
                })?;
                let attempt = ExecutionAttempt {
    input_projection: None,
                    attempt_id: "original-claim".into(), agent_id: "target".into(),
                    source_message_id: Some(message.id.clone()),
                    source: ExecutionSource {
                        identity: ExecutionSourceIdentity::QueueMessage { message_id: message.id.clone() },
                        generation: message.message_seq.unwrap(),
                    },
                    binding: ExecutionBinding::AgentLifecycle { agent_id: "target".into() },
                    provenance: ExecutionProvenance {
                        origin: ExecutionOrigin::Task, trust: ExecutionTrust::RuntimeInstruction,
                        priority: ExecutionPriority::Normal, correlation_id: None, causation_id: None,
                    },
                    admitted_fences: AdmittedFences {
                        source_revision: message.message_seq.unwrap(), work_item_source_revision: None,
                        work_item_generation: None, rejoin: None, agent_control_revision: 1,
                        host_registry_revision: 1,
                    },
                    state: ExecutionAttemptState::Open, run_id: None, turn_id: None,
                    recovery_of_attempt_id: None, terminal_outcome_id: None,
                    admitted_at: message.created_at.to_rfc3339(), terminal_at: None,
                };
                let mut execution = ExecutionProtocolState::empty("target");
                execution.attempts.insert(attempt.attempt_id.clone(), attempt.clone());
                crate::runtime_db::transitions::persist_state_tx(tx, &execution)?;
                let mut delivery = prepared.record;
                delivery.outcome = AgentMessageDeliveryOutcome::Accepted;
                delivery.state = AgentMessageDeliveryState::Dispatched;
                delivery.activation_id = Some(attempt.attempt_id.clone());
                match variant {
                    "activation" => delivery.activation_id = Some("different-claim".into()),
                    "queued" => delivery.state = AgentMessageDeliveryState::Queued,
                    "caller" => delivery.caller.caller_agent_id = Some("target".into()),
                    "bindings" => delivery.caller.current_turn_id = Some("different-turn".into()),
                    _ => {},
                }
                if variant != "missing" {
                    crate::runtime_db::agent_message_delivery::insert_delivery_tx(tx, &delivery)?;
                }
                let actual = crate::runtime_db::transitions::execution_protocol_repository::load_state_unchecked_tx(tx, "target")?;
                let proof = observe(tx, None, &actual.attempts[&attempt.attempt_id], None);
                if variant == "exact" {
                    assert!(proof?.turn_keys.is_empty());
                } else {
                    assert!(proof.is_err(), "{variant} must not grant claim-only isolation");
                }
                let stored: String = tx.query_row("SELECT payload_json FROM messages WHERE message_id=?1", [&message.id], |row| row.get(0))?;
                assert_eq!(serde_json::from_str::<MessageEnvelope>(&stored)?.turn_id, caller.current_turn_id);
                Ok(())
            })?;
        }
        Ok(())
    }
}
