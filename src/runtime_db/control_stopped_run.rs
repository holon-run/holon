//! Same-view canonical Stop evidence; this never proves physical completion.
use super::*;
use crate::runtime_db::episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey};

/// This is an observed immutable address, not a current State/activation permit.
pub(crate) struct StoppedDispatchAddress {
    turn_id: String,
    turn_index: u64,
}
impl StoppedDispatchAddress {
    pub(crate) fn turn_id(&self) -> &str {
        &self.turn_id
    }
    pub(crate) fn turn_index(&self) -> u64 {
        self.turn_index
    }
}

/// Private fields keep canonical proof construction inside the borrowed view.
/// Pending/NeedsAttention remain observations. A runtime must separately borrow
/// its original completed ControlRunLease before minting an ended token.
pub(crate) struct CanonicalStoppedRunProof<'view> {
    connection: &'view Connection,
    receipt: ControlReceipt,
    obligation: ControlCancellationObligation,
    dispatch: Option<StoppedDispatchAddress>,
    source_message_id: String,
    source_sequence: u64,
}
impl CanonicalStoppedRunProof<'_> {
    pub(crate) fn receipt(&self) -> &ControlReceipt {
        &self.receipt
    }
    pub(crate) fn obligation(&self) -> &ControlCancellationObligation {
        &self.obligation
    }
    pub(crate) fn dispatch(&self) -> Option<&StoppedDispatchAddress> {
        self.dispatch.as_ref()
    }
    pub(crate) fn source_message_id(&self) -> &str {
        &self.source_message_id
    }
    pub(crate) fn source_sequence(&self) -> u64 {
        self.source_sequence
    }
    pub(crate) fn require_same_transaction(&self, tx: &Transaction<'_>) -> Result<()> {
        ensure!(
            std::ptr::eq(self.connection, &**tx),
            "Control Stop proof belongs to another transaction"
        );
        Ok(())
    }
    pub(crate) fn matches_memory_dispatch(
        &self,
        key: &EpisodeMemoryKey,
        invocation: &EpisodeMemoryInvocation,
    ) -> Result<()> {
        let turn = self
            .dispatch
            .as_ref()
            .context("claim-only Stop has no memory dispatch")?;
        ensure!(
            key.agent_id == self.receipt.key.agent_id
                && key.identity_incarnation == self.receipt.key.identity_incarnation
                && key.actual_dispatch_turn_id == turn.turn_id
                && invocation.message_id == self.source_message_id
                && invocation.turn_index == turn.turn_index,
            "Control Stop proof differs from original memory dispatch"
        );
        Ok(())
    }
}

/// Exact original command only. This does not inspect or clear any owner gate.
pub(crate) fn prove_committed_control_stop_for_run_tx<'view>(
    tx: &'view Transaction<'_>,
    key: &ControlKey,
    original: &ControlStoppedRun<'_>,
) -> Result<Option<CanonicalStoppedRunProof<'view>>> {
    prove_committed_control_stop_for_run_in(tx, key, original)
}

/// Locate only immutable original Stop keys after the caller has observed its
/// actual original run lease completion. No latest State or full partition read.
pub(crate) fn find_committed_control_stop_for_run_tx<'view>(
    tx: &'view Transaction<'_>,
    original: &ControlStoppedRun<'_>,
) -> Result<Option<CanonicalStoppedRunProof<'view>>> {
    find_committed_control_stop_for_run_core(tx, original)
}

fn validate_original(original: &ControlStoppedRun<'_>) -> Result<()> {
    ensure!(
        original.source.agent_id == original.agent_id
            && original.source.message_seq.is_some_and(|seq| seq > 0)
            && original.identity_incarnation > 0
            && original.owner_epoch > 0
            && !original.run_id.is_empty()
            && !original.runtime_lease_id.is_empty(),
        "original Control run evidence is incomplete"
    );
    Ok(())
}

/// Compatibility wrapper may locate the original command; new finish callers
/// already have its exact key. Both execute the same canonical validator.
pub(super) fn find_committed_control_stop_for_run_in<'view>(
    connection: &'view Connection,
    original: &ControlStoppedRun<'_>,
) -> Result<Option<CanonicalStoppedRunProof<'view>>> {
    let proof = find_committed_control_stop_for_run_core(connection, original)?;
    if proof.is_some() {
        // Preserve the compatibility API's whole-partition corruption check.
        execution_protocol_repository::context_execution_protocol_on(
            connection,
            original.agent_id,
        )?;
    }
    Ok(proof)
}

fn find_committed_control_stop_for_run_core<'view>(
    connection: &'view Connection,
    original: &ControlStoppedRun<'_>,
) -> Result<Option<CanonicalStoppedRunProof<'view>>> {
    validate_original(original)?;
    let mut query = connection.prepare(
        "SELECT DISTINCT command_id FROM control_cancellation_obligations
        WHERE agent_id=?1 AND identity_incarnation=?2 AND target_kind='run' AND target_identity=?3
        ORDER BY command_id LIMIT 65537",
    )?;
    let ids = query
        .query_map(
            params![
                original.agent_id,
                i64::try_from(original.identity_incarnation)?,
                original.run_id
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        ids.len() <= 65536,
        "Control Stop command inventory exceeds preparation limit"
    );
    for command_id in ids {
        let key = ControlKey {
            agent_id: original.agent_id.into(),
            identity_incarnation: original.identity_incarnation,
            command_id,
        };
        if let Some(proof) = prove_committed_control_stop_for_run_in(connection, &key, original)? {
            return Ok(Some(proof));
        }
    }
    Ok(None)
}

fn prove_committed_control_stop_for_run_in<'view>(
    connection: &'view Connection,
    key: &ControlKey,
    original: &ControlStoppedRun<'_>,
) -> Result<Option<CanonicalStoppedRunProof<'view>>> {
    validate_original(original)?;
    validate_key(key)?;
    ensure!(
        key.agent_id == original.agent_id
            && key.identity_incarnation == original.identity_incarnation,
        "original Stop command belongs to another scope"
    );
    let obligations = load_obligations_in(connection, key)?;
    let Some(obligation) = obligations.iter().find(|obligation| {
        let target = &obligation.target;
        target.target_kind == ControlCancellationKind::Run
            && target.target_identity == original.run_id
            && target.target_generation.get("runtime_lease_id")
                == Some(&json!(original.runtime_lease_id))
            && target.target_generation.get("owner_epoch") == Some(&json!(original.owner_epoch))
            && target.target_generation.get("activation_id") == Some(&json!(original.activation_id))
            && target.target_generation.get("source_message_id") == Some(&json!(original.source.id))
            && target.target.get("reason") == Some(&json!("agent_stopped"))
    }) else {
        return Ok(None);
    };
    let payload: String = connection.query_row(
        "SELECT receipt_json FROM control_command_receipts
    WHERE agent_id = ?1 AND identity_incarnation = ?2 AND command_id = ?3",
        params![
            key.agent_id,
            i64::try_from(key.identity_incarnation)?,
            key.command_id
        ],
        |row| row.get(0),
    )?;
    let unchecked: ControlReceipt = serde_json::from_str(&payload)?;
    let receipt = load_control_receipt_in(
        connection,
        &key,
        &unchecked.invocation,
        Some(&unchecked.command_digest),
    )?
    .context("original Stop receipt is missing")?;
    ensure!(
        receipt.invocation.action == ControlAction::Stop
            && receipt.outcome.action == ControlAction::Stop
            && receipt.outcome.original_current_run_id.as_deref() == Some(original.run_id)
            && receipt.outcome.aborted_run_id.as_deref() == Some(original.run_id)
            && receipt.outcome.status == AgentStatus::Stopped
            && receipt.outcome.current_run_id.is_none()
            && receipt
                .obligation_ids
                .contains(&obligation.target.obligation_id)
            && receipt
                .outcome
                .aborted_queue_message_ids
                .contains(&original.source.id),
        "original Stop receipt does not settle this run source"
    );
    let (payload, seq): (String, u64) = connection.query_row(
        "SELECT payload_json,message_seq FROM messages WHERE agent_id = ?1 AND message_id = ?2",
        params![original.agent_id, original.source.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let source: MessageEnvelope = serde_json::from_str(&payload)?;
    ensure!(
        source == *original.source && Some(seq) == source.message_seq,
        "original Stop source no longer matches canonical evidence"
    );
    let payload: String = connection.query_row(
        "SELECT payload_json FROM queue_entries WHERE agent_id = ?1 AND message_id = ?2",
        params![original.agent_id, source.id],
        |row| row.get(0),
    )?;
    let queue: QueueEntryRecord = serde_json::from_str(&payload)?;
    ensure!(
        queue.agent_id == original.agent_id
            && queue.message_id == source.id
            && queue.priority == source.priority
            && queue.created_at == source.created_at
            && queue.status == QueueEntryStatus::Aborted
            && queue.updated_at == receipt.committed_at,
        "original Stop queue terminal facts disagree"
    );
    let attempt = if let Some(activation_id) = original.activation_id {
        let initialized: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_protocol_partitions WHERE agent_id=?1)",
            [original.agent_id],
            |row| row.get(0),
        )?;
        ensure!(initialized, "original Stop attempt partition is missing");
        let attempt = load_stopped_attempt(connection, original.agent_id, activation_id)?;
        let outcome_id = format!(
            "control:{}:interrupt:{activation_id}",
            digest(&serde_json::to_value(key)?)?
        );
        let outcome = load_stopped_outcome(connection, original.agent_id, &outcome_id)?;
        ensure!(
            attempt.agent_id == original.agent_id
                && attempt.source_message_id.as_deref() == Some(source.id.as_str())
                && attempt
                    .run_id
                    .as_deref()
                    .is_none_or(|run| run == original.run_id)
                && attempt.state == ExecutionAttemptState::Interrupted
                && attempt.terminal_outcome_id.as_deref() == Some(outcome_id.as_str())
                && attempt.terminal_at.as_deref()
                    == Some(receipt.committed_at.to_rfc3339().as_str())
                && outcome.attempt_id == activation_id
                && outcome.created_at == receipt.committed_at.to_rfc3339()
                && receipt
                    .outcome
                    .interrupted_attempt_ids
                    .iter()
                    .any(|id| id == activation_id),
            "original Stop attempt terminal facts disagree"
        );
        validate_interrupted_pair(&attempt, &outcome)?;
        Some(attempt)
    } else {
        None
    };
    // Validate every receipt-declared Turn, including claim-only cases;
    // a missing declared Turn must not masquerade as an unstarted run.
    let declared_turns = records_by_ids::<TurnRecord>(
        connection,
        "turn_records",
        "turn_id",
        &receipt.outcome.interrupted_turn_ids,
    )?;
    let turns = {
        let mut query = connection.prepare("SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND run_id = ?2 ORDER BY turn_id LIMIT 2")?;
        let rows = query.query_map(params![original.agent_id, original.run_id], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|payload| serde_json::from_str::<TurnRecord>(&payload).map_err(Into::into))
            .collect::<Result<Vec<_>>>()?
    };
    ensure!(
        turns.len() <= 1,
        "original Stop run has multiple canonical Turns"
    );
    if let Some(turn) = turns.first() {
        ensure!(
            turn.agent_id == original.agent_id
                && turn.run_id.as_deref() == Some(original.run_id)
                && turn.trigger.as_ref()
                    == Some(&crate::types::TurnTriggerSummary::from_message(&source))
                && turn.input_message_ids.contains(&source.id)
                && declared_turns.iter().any(|declared| declared == turn)
                && turn
                    .terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.kind == TurnTerminalKind::Interrupted
                        && terminal.reason.as_deref() == Some("agent_stopped")
                        && terminal.completed_at == receipt.committed_at)
                && attempt.as_ref().is_none_or(|attempt| attempt
                    .turn_id
                    .as_deref()
                    .is_none_or(|id| id == turn.turn_id)),
            "original Stop Turn terminal facts disagree"
        );
    } else {
        ensure!(
            attempt.is_some(),
            "reducer-only Stop proof requires its started canonical Turn"
        );
    }
    let dispatch = turns.first().map(|turn| StoppedDispatchAddress {
        turn_id: turn.turn_id.clone(),
        turn_index: turn.turn_index,
    });
    Ok(Some(CanonicalStoppedRunProof {
        connection,
        receipt,
        obligation: obligation.clone(),
        dispatch,
        source_sequence: seq,
        source_message_id: source.id,
    }))
}

fn load_stopped_attempt(
    connection: &Connection,
    agent: &str,
    id: &str,
) -> Result<execution_protocol::ExecutionAttempt> {
    let (state,source,generation,predecessor,outcome,payload):
        (String,String,u64,Option<String>,Option<String>,String)=connection.query_row(
        "SELECT lifecycle_state,source_identity_json,source_generation,recovery_of_attempt_id,terminal_outcome_id,payload_json
         FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",
        params![agent,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    let attempt: execution_protocol::ExecutionAttempt = serde_json::from_str(&payload)?;
    ensure!(
        attempt.agent_id == agent
            && attempt.attempt_id == id
            && serde_json::to_value(attempt.state)?.as_str() == Some(state.as_str())
            && serde_json::from_str::<execution_protocol::ExecutionSourceIdentity>(&source)?
                == attempt.source.identity
            && generation == attempt.source.generation
            && predecessor == attempt.recovery_of_attempt_id
            && outcome == attempt.terminal_outcome_id,
        "original Stop Attempt columns and payload disagree"
    );
    Ok(attempt)
}
fn load_stopped_outcome(
    connection: &Connection,
    agent: &str,
    id: &str,
) -> Result<execution_protocol::ExecutionOutcomeRecord> {
    let (attempt_id,payload):(String,String)=connection.query_row(
        "SELECT attempt_id,payload_json FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id=?2",
        params![agent,id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let outcome: execution_protocol::ExecutionOutcomeRecord = serde_json::from_str(&payload)?;
    ensure!(
        outcome.outcome_id == id && outcome.attempt_id == attempt_id,
        "original Stop Outcome columns and payload disagree"
    );
    Ok(outcome)
}

fn validate_interrupted_pair(
    attempt: &execution_protocol::ExecutionAttempt,
    outcome: &execution_protocol::ExecutionOutcomeRecord,
) -> Result<()> {
    use execution_protocol::{
        CommandResult, ConversationOutcome, ExecutionOutcome, WorkItemOutcome,
    };
    let expected = match &attempt.binding {
        ExecutionBinding::WorkItem { .. } => {
            ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted {
                reason: "agent_stopped".into(),
            })
        }
        ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. } => {
            ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                reason: "agent_stopped".into(),
            })
        }
        ExecutionBinding::Command => ExecutionOutcome::Command(CommandResult::Applied {
            references: vec![format!("interrupted:{}", attempt.attempt_id)],
        }),
    };
    ensure!(
        outcome.outcome == expected,
        "original Stop outcome differs from canonical interruption"
    );
    let mut partition = ExecutionProtocolState::empty(&attempt.agent_id);
    partition
        .attempts
        .insert(attempt.attempt_id.clone(), attempt.clone());
    partition
        .outcomes
        .insert(outcome.outcome_id.clone(), outcome.clone());
    execution_protocol::assert_invariants(&partition)
        .map_err(|error| anyhow!("original Stop reciprocal evidence is invalid: {error}"))?;
    Ok(())
}
