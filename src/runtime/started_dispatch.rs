//! Existing assignment is history; a new dispatch needs its original live claim.
use super::{
    control_cancellation::ControlRunLease,
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_input_evidence::{
        CapturedInputEvidence, EvidenceLocator, InputEvidenceFamily as Family,
        PreparedInputEvidence,
    },
    state_mutation::RuntimeMutation,
    RuntimeHandle,
};
use crate::{
    domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding},
    runtime_db::{
        dispatch_memory::load_dispatch_memory_binding_tx,
        input_evidence_snapshot::validate_existing_input_evidence_tx,
        input_mutation_receipts::{
            lookup_input_mutation_receipt_tx, InputMutationFamily, InputMutationResult,
            PreparedInputMutation,
        },
        transitions::AgentPostureTransitionCommand,
    },
    types::{
        AgentState, ExecutionAdmissionProvenance, MessageEnvelope, MessageKind, TurnOwner,
        TurnRecord, WorkItemExecutionBinding,
    },
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Transaction};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("actual dispatch conflict: {0}")]
pub(crate) struct StartedDispatchConflict(pub(crate) &'static str);

/// No deserializer or public constructor: an address cannot recreate a live producer.
pub(crate) struct OriginalDispatchClaim {
    original: Arc<ControlRunLease>,
    source: EvidenceLocator,
    source_sequence: u64,
    source_kind: MessageKind,
    attempt: Option<EvidenceLocator>,
    previous_terminal: Option<EvidenceLocator>,
    owner: Option<TurnOwner>,
    claimed_work_revision: Option<u64>,
    _budget: Arc<BudgetViewGuard>,
}
impl std::fmt::Debug for OriginalDispatchClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalDispatchClaim")
            .field("run_id", &self.original.run_id())
            .field("lease_id", &self.original.lease_id())
            .finish()
    }
}
// Mirror the existing StartedTurn builder without allocating an owner copy.
fn dispatch_owner_matches(
    binding: &WorkItemExecutionBinding,
    turn: &TurnRecord,
    agent: &str,
) -> bool {
    turn.current_work_item_id == binding.work_item_id
        && match &binding.owner {
            Some(owner) => {
                turn.owner.as_ref() == Some(owner)
                    && owner.work_item_id() == binding.work_item_id.as_deref()
            }
            None => match (binding.work_item_id.as_deref(), turn.owner.as_ref()) {
                (Some(expected), Some(TurnOwner::WorkItem { work_item_id })) => {
                    work_item_id == expected
                }
                (None, Some(TurnOwner::AgentLifecycle { agent_id })) => agent_id == agent,
                _ => false,
            },
        }
}
impl OriginalDispatchClaim {
    pub(crate) fn proof_material_tx(&self, tx: &Transaction<'_>) -> Result<(usize, usize)> {
        let bytes: usize = tx.query_row(
            "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE evidence_id=?2 AND agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?3),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM turn_records WHERE turn_id=?4),0)",
            params![self.source.scope.agent_id,self.source.key.id,self.original.activation_id(),self.previous_terminal.as_ref().map(|turn|turn.key.id.as_str())], |r| r.get(0),
        )?;
        Ok((bytes, 5))
    }
    fn validate_locator_tx(&self, tx: &Transaction<'_>, locator: &EvidenceLocator) -> Result<()> {
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 AND availability='present' AND revision=?5 AND content_digest=?6)",
            params![locator.scope.agent_id,locator.scope.identity_incarnation,locator.key.family.as_str(),locator.key.id,locator.revision,locator.content_digest], |r| r.get(0),
        )?;
        ensure!(current, "original dispatch input observation changed");
        validate_existing_input_evidence_tx(tx, &locator.scope, &locator.key)
    }
    fn validate_current_tx(&self, tx: &Transaction<'_>, state: &AgentState) -> Result<()> {
        ensure!(
            self.original.is_live(),
            StartedDispatchConflict("original dispatch lease is no longer live")
        );
        ensure!(
            state.id == self.source.scope.agent_id
                && state.status != crate::types::AgentStatus::Stopped
                && state.current_run_id.as_deref() == Some(self.original.run_id()),
            StartedDispatchConflict("original dispatch differs from current run")
        );
        let source_matches: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE evidence_id=?1 AND agent_id=?2 AND message_seq=?3 AND kind=?4 AND json_extract(payload_json,'$.message_seq')=?3 AND json_extract(payload_json,'$.id')=?1 AND json_extract(payload_json,'$.agent_id')=?2 AND json_extract(payload_json,'$.kind')=?4)",
            params![self.source.key.id,self.source.scope.agent_id,self.source_sequence,serde_json::to_value(&self.source_kind)?.as_str()], |r| r.get(0),
        )?;
        ensure!(source_matches, "original dispatch source changed");
        let dequeued: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1 AND agent_id=?2 AND status='dequeued' AND json_extract(payload_json,'$.message_id')=?1 AND json_extract(payload_json,'$.agent_id')=?2 AND json_extract(payload_json,'$.status')='dequeued')",
            params![self.source.key.id,self.source.scope.agent_id], |r| r.get(0),
        )?;
        ensure!(dequeued, "original dispatch has no current dequeued claim");
        self.validate_locator_tx(tx, &self.source)?;
        let predecessor: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts WHERE agent_id=?1 AND lifecycle_state='open' AND json_extract(payload_json,'$.source_message_id')=?2 AND (?3 IS NULL OR attempt_id!=?3))",
            params![self.source.scope.agent_id,self.source.key.id,self.original.activation_id()], |r| r.get(0),
        )?;
        ensure!(
            !predecessor,
            StartedDispatchConflict("original dispatch has another Open predecessor")
        );
        if let Some(attempt) = &self.attempt {
            self.validate_locator_tx(tx, attempt)?;
            let admitted: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2 AND lifecycle_state='open' AND json_extract(payload_json,'$.agent_id')=?1 AND json_extract(payload_json,'$.attempt_id')=?2 AND json_extract(payload_json,'$.state')='open' AND json_extract(payload_json,'$.source_message_id')=?3 AND json_extract(payload_json,'$.admitted_fences.source_revision')=?4 AND (json_extract(payload_json,'$.run_id') IS NULL OR json_extract(payload_json,'$.run_id')=?5))",
                params![self.source.scope.agent_id,attempt.key.id,self.source.key.id,self.source_sequence,self.original.run_id()], |r| r.get(0),
            )?;
            ensure!(admitted, "original dispatch canonical claim changed");
        }
        Ok(())
    }
    pub(crate) fn validate_posture_tx(
        &self,
        tx: &Transaction<'_>,
        command: &AgentPostureTransitionCommand,
        turn: &TurnRecord,
    ) -> Result<()> {
        let before = command
            .agent_state
            .expected
            .as_deref()
            .context("original dispatch requires its exact pre-State")?;
        match (&before.current_execution_binding, &self.previous_terminal) {
            (None, None) => {}
            (Some(binding), Some(previous)) => {
                ensure!(
                    before.current_turn_id.as_deref() == Some(previous.key.id.as_str())
                        && binding.turn_id == previous.key.id,
                    StartedDispatchConflict("new dispatch previous binding changed")
                );
                self.validate_locator_tx(tx, previous)?;
                let finished: bool = tx.query_row(
                    "SELECT agent_id=?2 AND json_type(payload_json,'$.terminal')='object' AND json_extract(payload_json,'$.run_id') IS NOT ?3 FROM turn_records WHERE turn_id=?1",
                    params![previous.key.id,self.source.scope.agent_id,self.original.run_id()], |r|r.get(0),
                )?;
                ensure!(
                    finished,
                    StartedDispatchConflict("new dispatch cannot replace an active binding")
                );
            }
            _ => anyhow::bail!(StartedDispatchConflict(
                "new dispatch previous binding changed"
            )),
        }
        self.validate_current_tx(tx, before)?;
        let after = command.agent_state.record.as_ref();
        let binding = after
            .current_execution_binding
            .as_ref()
            .context("new dispatch has no actual binding")?;
        ensure!(
            command.agent_id == self.source.scope.agent_id
                && after.current_run_id.as_deref() == Some(self.original.run_id())
                && after.turn_index
                    == before
                        .turn_index
                        .checked_add(1)
                        .context("Turn index overflow")?
                && after.current_turn_id.as_deref() == Some(turn.turn_id.as_str())
                && turn.agent_id == after.id
                && turn.turn_index == after.turn_index
                && turn.run_id == after.current_run_id
                && turn.terminal.is_none()
                && binding.turn_id == turn.turn_id
                && binding.source_message_id == self.source.key.id
                && binding.activation_id.as_deref() == self.original.activation_id()
                && dispatch_owner_matches(binding, turn, &after.id)
                && turn
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref())
                    == Some(self.source.key.id.as_str())
                && turn.input_message_ids.len() == 1
                && turn.input_message_ids.first().map(String::as_str)
                    == Some(self.source.key.id.as_str()),
            StartedDispatchConflict("new dispatch differs from its original claim/Turn")
        );
        if let Some(owner) = &self.owner {
            ensure!(
                binding.owner.as_ref() == Some(owner)
                    && turn.owner.as_ref() == Some(owner)
                    && binding.claimed_work_revision == self.claimed_work_revision,
                StartedDispatchConflict(
                    "new dispatch changed its canonical admission owner/fences"
                )
            );
        }
        Ok(())
    }
}

fn validate_original_start_receipt_tx(
    tx: &Transaction<'_>,
    inputs: &CapturedInputEvidence,
    turn: &TurnRecord,
    source: &MessageEnvelope,
) -> Result<()> {
    // Caller has charged the header, descriptor and all original receipt fields.
    let descriptor: String = tx.query_row(
        "SELECT json_extract(payload_json,'$.original_start') FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
        params![inputs.scope.agent_id,inputs.scope.identity_incarnation,turn.turn_id], |r|r.get(0),
    )?;
    let descriptor: PreparedInputMutation = serde_json::from_str(&descriptor)?;
    ensure!(
        descriptor.family() == InputMutationFamily::AgentPosture
            && descriptor.scopes() == std::slice::from_ref(&inputs.scope),
        StartedDispatchConflict("retry original Start has another posture scope")
    );
    let receipt = lookup_input_mutation_receipt_tx(tx, &descriptor)?
        .context("retry original whole Start receipt is missing")?;
    let acknowledged = &receipt.outcome.scopes[0];
    ensure!(
        receipt.outcome.applied
            && matches!(receipt.outcome.result, InputMutationResult::Transition)
            && acknowledged.scope == inputs.scope
            && acknowledged
                .install_keys
                .input_roots
                .iter()
                .any(|key| key.family == Family::Turn && key.id == turn.turn_id)
            && acknowledged
                .install_keys
                .input_roots
                .iter()
                .any(|key| key.family == Family::Message && key.id == source.id)
            && acknowledged
                .audit_events
                .iter()
                .any(|event| event.id == descriptor.command_nonce()
                    && event.kind == "agent_state_mutated"),
        StartedDispatchConflict("retry whole Start ACK differs from actual Turn/State address")
    );
    Ok(())
}

impl RuntimeHandle {
    pub(super) fn original_dispatch_claim(
        &self,
        mutation: &RuntimeMutation,
        source: &MessageEnvelope,
        inputs: &CapturedInputEvidence,
        provenance: Option<&ExecutionAdmissionProvenance>,
    ) -> Result<OriginalDispatchClaim> {
        self.capture_dispatch_claim(mutation, source, inputs, provenance, false)
    }

    fn capture_dispatch_claim(
        &self,
        mutation: &RuntimeMutation,
        source: &MessageEnvelope,
        inputs: &CapturedInputEvidence,
        provenance: Option<&ExecutionAdmissionProvenance>,
        retry: bool,
    ) -> Result<OriginalDispatchClaim> {
        let run = mutation
            .state
            .current_run_id
            .as_deref()
            .context("historical assignment requires an actual current claim")?;
        let handle = mutation
            .current_run_abort
            .as_ref()
            .context("original dispatch has no original handle")?;
        ensure!(
            handle.run_id == run && !handle.token.is_cancelled(),
            StartedDispatchConflict("original dispatch handle is not live")
        );
        let original = self
            .inner
            .control_cancellations
            .run_for(run)
            .context("original dispatch has no completion lease")?;
        ensure!(
            original.is_live() && original.source_message_id() == source.id,
            StartedDispatchConflict("original dispatch lease/source differs")
        );
        #[allow(deprecated)]
        let activation = match provenance {
            Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) => {
                Some(activation_id.as_str())
            }
            Some(ExecutionAdmissionProvenance::LegacyCompat { .. }) => {
                anyhow::bail!("legacy admission cannot authorize a new dispatch")
            }
            None => None,
        };
        ensure!(
            activation == original.activation_id(),
            StartedDispatchConflict("original dispatch activation differs from its live lease")
        );
        let observed_source = inputs
            .records
            .iter()
            .find(|record| {
                record.locator.key.family == Family::Message && record.locator.key.id == source.id
            })
            .context("original dispatch source has not been prepared")?;
        let PreparedInputEvidence::Message(message) = &observed_source.record else {
            anyhow::bail!("original dispatch source family differs")
        };
        ensure!(
            inputs.scope.agent_id == mutation.state.id
                && inputs.scope.identity_incarnation == self.inner.identity_incarnation
                && observed_source.locator.scope == inputs.scope
                && message.agent_id == source.agent_id
                && message.id == source.id
                && message.message_seq == source.message_seq
                && message.kind == source.kind,
            StartedDispatchConflict("original dispatch source observation differs")
        );
        let source_sequence = message
            .message_seq
            .context("original dispatch source has no actual sequence")?;
        let complete = inputs.execution.source(&source.id)?;
        ensure!(
            complete
                .attempts
                .values()
                .filter(|attempt| !attempt.pruned && attempt.state == ExecutionAttemptState::Open)
                .all(|attempt| Some(attempt.attempt_id.as_str()) == activation),
            StartedDispatchConflict("original dispatch has another Open predecessor")
        );
        if let Some(activation) = activation {
            ensure!(
                complete.attempts.get(activation).is_some_and(
                    |attempt| !attempt.pruned && attempt.state == ExecutionAttemptState::Open
                ),
                StartedDispatchConflict("canonical claim is absent from complete source coverage")
            );
        }
        let observed_attempt = activation
            .map(|id| {
                inputs
                    .records
                    .iter()
                    .find(|record| {
                        record.locator.key.family == Family::Attempt && record.locator.key.id == id
                    })
                    .context("original dispatch attempt has not been prepared")
            })
            .transpose()?;
        let previous_terminal = if !retry {
            mutation
                .state
                .current_execution_binding
                .as_ref()
                .map(|binding| {
                    let record = inputs
                        .records
                        .iter()
                        .find(|record| {
                            record.locator.key.family == Family::Turn
                                && record.locator.key.id == binding.turn_id
                        })
                        .context("new dispatch previous binding Turn has not been prepared")?;
                    let PreparedInputEvidence::Turn(turn) = &record.record else {
                        anyhow::bail!("previous binding Turn family differs")
                    };
                    ensure!(
                        mutation.state.current_turn_id.as_deref() == Some(turn.turn_id.as_str())
                            && record.locator.scope == inputs.scope
                            && turn.agent_id == source.agent_id
                            && turn.terminal.is_some()
                            && turn.run_id.as_deref() != Some(run),
                        StartedDispatchConflict("new dispatch cannot replace an active binding")
                    );
                    Ok::<_, anyhow::Error>(&record.locator)
                })
                .transpose()?
        } else {
            None
        };
        // Reserve the address/owner copies before cloning any of them.
        let bytes = serialized_bytes(&(
            &observed_source.locator,
            &source.kind,
            observed_attempt.map(|record| (&record.locator, &record.record)),
            previous_terminal,
        ))?
        .checked_mul(4)
        .context("dispatch claim metadata overflow")?;
        let budget = self.inner.input_evidence_budget.register_owned_bytes(
            &inputs.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let (owner, claimed_work_revision) = if let Some(record) = observed_attempt {
            let PreparedInputEvidence::Attempt(attempt) = &record.record else {
                anyhow::bail!("original dispatch attempt family differs")
            };
            ensure!(
                record.locator.scope == inputs.scope
                    && attempt.state == ExecutionAttemptState::Open
                    && attempt.agent_id == source.agent_id
                    && attempt.source_message_id.as_deref() == Some(source.id.as_str())
                    && attempt.admitted_fences.source_revision == source_sequence
                    && attempt.run_id.as_deref().is_none_or(|id| id == run),
                StartedDispatchConflict("original dispatch claim observation differs")
            );
            let owner = match &attempt.binding {
                ExecutionBinding::WorkItem { work_item_id } => TurnOwner::WorkItem {
                    work_item_id: work_item_id.clone(),
                },
                ExecutionBinding::Conversation { interaction_id } => TurnOwner::Conversation {
                    interaction_id: interaction_id.clone(),
                },
                ExecutionBinding::AgentLifecycle { agent_id } => TurnOwner::AgentLifecycle {
                    agent_id: agent_id.clone(),
                },
                ExecutionBinding::Command => TurnOwner::Command,
            };
            (
                Some(owner),
                attempt.admitted_fences.work_item_source_revision,
            )
        } else {
            (None, None)
        };
        Ok(OriginalDispatchClaim {
            original,
            source: observed_source.locator.clone(),
            source_sequence,
            source_kind: source.kind.clone(),
            attempt: observed_attempt.map(|record| record.locator.clone()),
            previous_terminal: previous_terminal.cloned(),
            owner,
            claimed_work_revision,
            _budget: budget,
        })
    }

    pub(super) fn retry_started_dispatch(
        &self,
        mutation: &RuntimeMutation,
        source: &MessageEnvelope,
        inputs: &CapturedInputEvidence,
        provenance: Option<&ExecutionAdmissionProvenance>,
    ) -> Result<Option<(String, u64)>> {
        let Some(binding) = mutation
            .state
            .current_execution_binding
            .as_ref()
            .filter(|binding| binding.source_message_id == source.id)
        else {
            return Ok(None);
        };
        ensure!(
            mutation.state.current_turn_id.as_deref() == Some(binding.turn_id.as_str()),
            StartedDispatchConflict("retry current Turn differs from its binding")
        );
        let record = inputs
            .records
            .iter()
            .find(|record| {
                record.locator.key.family == Family::Turn
                    && record.locator.key.id == binding.turn_id
            })
            .context("retry actual Turn has not been prepared")?;
        let PreparedInputEvidence::Turn(turn) = &record.record else {
            anyhow::bail!("retry Turn family differs")
        };
        if turn.run_id != mutation.state.current_run_id {
            ensure!(
                turn.terminal.is_some(),
                StartedDispatchConflict("previous binding has an unfinished different run")
            );
            return Ok(None);
        }
        let claim = self.capture_dispatch_claim(mutation, source, inputs, provenance, true)?;
        ensure!(
            binding.activation_id.as_deref() == claim.original.activation_id()
                && binding.admission_provenance.as_ref() == provenance
                && dispatch_owner_matches(binding, turn, &mutation.state.id)
                && claim
                    .owner
                    .as_ref()
                    .is_none_or(|owner| binding.owner.as_ref() == Some(owner)
                        && binding.claimed_work_revision == claim.claimed_work_revision)
                && turn.agent_id == inputs.scope.agent_id
                && turn.run_id == mutation.state.current_run_id
                && turn.turn_index == mutation.state.turn_index
                && turn.terminal.is_none()
                && turn
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref())
                    == Some(source.id.as_str())
                && turn.input_message_ids.contains(&source.id),
            StartedDispatchConflict("retry differs from its actual current dispatch")
        );
        self.inner.runtime_db.transaction(|tx| {
            let (bytes, _) = claim.proof_material_tx(tx)?;
            let turn_bytes: usize = tx.query_row(
                "SELECT length(CAST(payload_json AS BLOB)) FROM turn_records WHERE turn_id=?1",
                [&turn.turn_id],
                |r| r.get(0),
            )?;
            let (phase, header_bytes): (String, usize) = tx.query_row(
                "SELECT state,COALESCE(length(CAST(json_object('key',json_extract(payload_json,'$.start.key'),'invocation',json_extract(payload_json,'$.start.invocation'),'source_sequence',json_extract(payload_json,'$.start.source_sequence'),'source_kind',json_extract(payload_json,'$.start.source_kind'),'digest',json_extract(payload_json,'$.start.digest'),'original_start',json_extract(payload_json,'$.original_start')) AS BLOB)),0)+COALESCE(length(CAST(json_extract(c.payload_json,'$.original_start') AS BLOB)),0)+COALESCE((SELECT length(CAST(family AS BLOB))+8+length(CAST(command_digest AS BLOB))+length(CAST(scope_digest AS BLOB))+length(CAST(scopes_json AS BLOB))+length(CAST(prepared_at AS BLOB))+length(CAST(prepared_json AS BLOB))+length(CAST(outcome_json AS BLOB))+length(CAST(committed_at AS BLOB))+length(CAST(result_digest AS BLOB)) FROM input_mutation_receipts WHERE command_nonce=json_extract(c.payload_json,'$.original_start.command_nonce')),0) FROM dispatch_memory_checkpoints c WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
                params![inputs.scope.agent_id,inputs.scope.identity_incarnation,turn.turn_id], |r|Ok((r.get(0)?,r.get(1)?)),
            )?;
            ensure!(matches!(phase.as_str(), "started" | "pending"),
                StartedDispatchConflict("retry requires its original unfinished Start"));
            let material = bytes.checked_add(turn_bytes).and_then(|n|n.checked_add(header_bytes))
                .context("retry proof material overflow")?;
            let charged = u64::try_from(material)?.checked_mul(8)
                .context("retry proof copies overflow")?;
            let limits = self.inner.input_evidence_budget.limits();
            let byte_limit = limits.history_per_agent_bytes.checked_add(limits.pending_per_agent_bytes)
                .context("retry owner byte limit overflow")?;
            ensure!(charged <= byte_limit && 18 * 8 <= limits.record_limit,
                crate::runtime_db::dispatch_memory::DispatchMemoryError::ResourceExhausted);
            let _budget = self.inner.input_evidence_budget.register_owned_bytes(
                &inputs.scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: charged,
                    metadata_estimate_bytes: 0,
                },
            )?;
            claim.validate_current_tx(tx, &mutation.state)?;
            claim.validate_locator_tx(tx, &record.locator)?;
            validate_original_start_receipt_tx(tx, inputs, turn, source)?;
            ensure!(
                load_dispatch_memory_binding_tx(tx, &inputs.scope, &mutation.state)?.is_some(),
                StartedDispatchConflict("retry has no original unfinished Start receipt")
            );
            Ok(())
        })?;
        Ok(Some((turn.turn_id.clone(), turn.turn_index)))
    }
}
