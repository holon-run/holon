//! Read-only original memory evidence for a Stop-owned provider-pause ACK.
use super::*;
use crate::{
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::{
        control_transition::CanonicalStoppedRunProof,
        dispatch_memory::{self, DispatchMemoryBinding, DispatchMemoryRecord},
        episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey},
        input_mutation_receipts::canonical_command_digest,
        task_snapshot::TaskDomainLoadRequest,
    },
};
use std::sync::Arc;

/// Sealed observation, not a Finish, absence exemption or producer-ended proof.
#[derive(Debug, serde::Serialize)]
pub(crate) struct StoppedProviderPauseMemory {
    original_digest: String,
    observed: Observation,
    #[serde(skip)]
    guards: Vec<Arc<BudgetViewGuard>>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
enum Observation {
    ClaimOnly,
    Dispatch {
        key: EpisodeMemoryKey,
        invocation: EpisodeMemoryInvocation,
        run_id: String,
        binding: Option<DispatchMemoryBinding>,
        record_digest: Option<String>,
        legacy: bool,
    },
}

fn original_digest(frozen: &FrozenStoppedProviderPause) -> Result<String> {
    canonical_command_digest(&(
        &frozen.agent_id,
        frozen.identity_incarnation,
        frozen.owner_epoch,
        &frozen.run_id,
        &frozen.runtime_lease_id,
        &frozen.activation_id,
        &frozen.source,
        &frozen.pause,
    ))
}

impl StoppedProviderPauseMemory {
    pub(super) fn require_original(&self, frozen: &FrozenStoppedProviderPause) -> Result<()> {
        ensure!(
            self.original_digest == original_digest(frozen)?,
            "stopped memory observation differs from its original ACK"
        );
        Ok(())
    }

    pub(super) fn require_budget(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        for guard in &self.guards {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok(())
    }
}

fn charge(
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
) -> Result<Arc<BudgetViewGuard>> {
    crate::runtime_db::command_task_terminal::charge_material(
        scope,
        bytes,
        records,
        &TaskDomainLoadRequest {
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            recovery_at: None,
            byte_limit: 64 * 1024 * 1024,
            record_limit: usize::try_from(ledger.limits().record_limit)?.min(65536),
        },
        ledger,
    )
}

pub(crate) fn load_stopped_provider_pause_memory_tx(
    tx: &Transaction<'_>,
    ack: &StoppedProviderPauseAck<'_>,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<StoppedProviderPauseMemory> {
    let frozen = ack.frozen;
    ensure!(
        scope.agent_id == frozen.agent_id
            && scope.identity_incarnation == frozen.identity_incarnation,
        "stopped memory observation has another scope"
    );
    let (bytes, records) = stopped_provider_pause_material_tx(tx, &frozen.stopped_run())?;
    let mut guards = vec![charge(scope, ledger, bytes, records)?];
    let proof = crate::runtime_db::control_transition::find_committed_control_stop_for_run_tx(
        tx,
        &frozen.stopped_run(),
    )?
    .context("stopped memory observation has no canonical Stop proof")?;
    proof.require_same_transaction(tx)?;
    frozen.require_original_ack_shape_tx(tx, ack.queue)?;
    ensure!(
        read_parking(tx, &frozen.agent_id, &frozen.source.id)?.as_ref()
            == Some(&frozen.pause.parking),
        "stopped provider pause parking changed before memory observation"
    );
    if let Some(dispatch) = proof.dispatch() {
        let (bytes, records) = dispatch_material_tx(tx, scope, dispatch.turn_id())?;
        guards.push(charge(scope, ledger, bytes, records)?);
    }
    Ok(StoppedProviderPauseMemory {
        original_digest: original_digest(frozen)?,
        observed: observe_tx(tx, frozen, &proof)?,
        guards,
    })
}

/// SQL sizes precede decoding either the seed, its original receipt or Done.
fn dispatch_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    turn_id: &str,
) -> Result<(usize, usize)> {
    let (bytes, records): (usize, usize) = tx.query_row(
        "SELECT COALESCE(SUM(bytes),0),COALESCE(SUM(records),0) FROM (
           SELECT length(CAST(payload_json AS BLOB)) AS bytes,
             1+COALESCE(json_array_length(payload_json,'$.obligation.members'),0) AS records
             FROM dispatch_memory_checkpoints
             WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3
           UNION ALL SELECT length(CAST(source_message_id AS BLOB))+length(CAST(turn_id AS BLOB))+32,1
             FROM dispatch_memory_legacy_dispatches
             WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3
           UNION ALL SELECT length(CAST(prepared_json AS BLOB))+length(CAST(outcome_json AS BLOB)),1
             FROM input_mutation_receipts WHERE command_nonce IN (
               SELECT json_extract(payload_json,'$.original_start.command_nonce')
               FROM dispatch_memory_checkpoints
               WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3)
           UNION ALL SELECT length(CAST(invocation_json AS BLOB))+length(CAST(result_json AS BLOB))
             +length(CAST(command_digest AS BLOB))+length(CAST(committed_at AS BLOB))
             +COALESCE(length(CAST(finalized_episode_id AS BLOB)),0),1
             FROM episode_memory_results
             WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3)",
        params![scope.agent_id, scope.identity_incarnation, turn_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((
        bytes
            .checked_mul(4)
            .context("stopped memory material overflow")?,
        records,
    ))
}

pub(super) fn frozen_material_tx(
    tx: &Transaction<'_>,
    frozen: &FrozenStoppedProviderPause,
) -> Result<(usize, usize)> {
    let memory = frozen
        .memory
        .as_ref()
        .context("stopped ACK omitted its memory observation")?;
    memory.require_original(frozen)?;
    match &memory.observed {
        Observation::ClaimOnly => Ok((0, 0)),
        Observation::Dispatch { key, .. } => dispatch_material_tx(
            tx,
            &InputEvidenceScope {
                agent_id: frozen.agent_id.clone(),
                identity_incarnation: frozen.identity_incarnation,
            },
            &key.actual_dispatch_turn_id,
        ),
    }
}

fn observe_tx(
    tx: &Transaction<'_>,
    frozen: &FrozenStoppedProviderPause,
    proof: &CanonicalStoppedRunProof<'_>,
) -> Result<Observation> {
    proof.require_same_transaction(tx)?;
    let Some(dispatch) = proof.dispatch() else {
        // The proof validated the original run has no Turn. Orphaned seeds
        // cannot be interpreted as a claim-only absence exemption.
        let orphaned: bool = tx.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM dispatch_memory_checkpoints c
               WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.source_message_id=?3
                 AND NOT EXISTS(SELECT 1 FROM turn_records t WHERE t.turn_id=c.turn_id)
               UNION ALL SELECT 1 FROM dispatch_memory_legacy_dispatches c
               WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.source_message_id=?3
                 AND NOT EXISTS(SELECT 1 FROM turn_records t WHERE t.turn_id=c.turn_id))",
            params![
                frozen.agent_id,
                frozen.identity_incarnation,
                frozen.source.id
            ],
            |row| row.get(0),
        )?;
        ensure!(
            !orphaned,
            "claim-only stopped ACK has an orphaned memory address"
        );
        return Ok(Observation::ClaimOnly);
    };
    let scope = InputEvidenceScope {
        agent_id: frozen.agent_id.clone(),
        identity_incarnation: frozen.identity_incarnation,
    };
    let key = EpisodeMemoryKey {
        agent_id: scope.agent_id.clone(),
        identity_incarnation: scope.identity_incarnation,
        actual_dispatch_turn_id: dispatch.turn_id().to_owned(),
    };
    let invocation = EpisodeMemoryInvocation {
        message_id: frozen.source.id.clone(),
        turn_index: dispatch.turn_index(),
    };
    proof.matches_memory_dispatch(&key, &invocation)?;
    // The canonical Stop proof already validated this exact Turn's original
    // run/source. This reader never uses the stopped current State's None run.
    let binding =
        dispatch_memory::load_dispatch_memory_binding_by_turn_tx(tx, &scope, dispatch.turn_id())?;
    if let Some(binding) = &binding {
        proof.matches_memory_dispatch(binding.key(), binding.invocation())?;
    }
    let record = dispatch_memory::load_dispatch_memory_tx(tx, &key, &invocation)?;
    let legacy = dispatch_memory::is_legacy_dispatch_tx(tx, &key, &invocation)?;
    ensure!(
        binding.is_some()
            || matches!(&record, Some(DispatchMemoryRecord::Done { .. }))
            || (record.is_none() && legacy),
        "stopped ACK has no validated seed, Done or legacy memory identity"
    );
    Ok(Observation::Dispatch {
        key,
        invocation,
        run_id: frozen.run_id.clone(),
        binding,
        record_digest: record.as_ref().map(canonical_command_digest).transpose()?,
        legacy,
    })
}

pub(super) fn revalidate_tx(
    tx: &Transaction<'_>,
    frozen: &FrozenStoppedProviderPause,
    proof: &CanonicalStoppedRunProof<'_>,
) -> Result<()> {
    let memory = frozen
        .memory
        .as_ref()
        .context("stopped ACK omitted its memory observation")?;
    memory.require_original(frozen)?;
    ensure!(
        memory.observed == observe_tx(tx, frozen, proof)?,
        "stopped ACK memory identity changed after preparation"
    );
    Ok(())
}
