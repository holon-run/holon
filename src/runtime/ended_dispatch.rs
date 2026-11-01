//! An original producer completion plus its original durable Stop proof.
use super::{control_cancellation::ControlRunLease, RuntimeHandle};
use crate::{
    runtime_db::{
        context_inputs::ContextDurableRead,
        control_transition::{self, ControlKey, ControlStoppedRun},
        dispatch_memory::{
            load_dispatch_memory_binding_by_turn_tx, DispatchMemoryBinding, DispatchMemoryIntent,
        },
        episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey},
        transitions::{AgentStateMutation, PreparedInputTransition, TurnTerminalTransitionCommand},
    },
    types::MessageEnvelope,
};
use anyhow::{ensure, Context, Result};
use rusqlite::Transaction;
use serde::Serialize;
use std::sync::Arc;

#[cfg(test)]
#[path = "ended_dispatch_tests.rs"]
mod tests;

/// No deserializer: a durable address cannot recreate its process capability.
#[derive(Serialize)]
pub(crate) struct PhysicalStoppedDispatchAuthorization {
    control_key: ControlKey,
    control_digest: String,
    owner_epoch: u64,
    source: Box<MessageEnvelope>,
    #[serde(skip)]
    original: Arc<ControlRunLease>,
}
impl std::fmt::Debug for PhysicalStoppedDispatchAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicalStoppedDispatchAuthorization")
            .field("control_key", &self.control_key)
            .field("original_run_id", &self.original.run_id())
            .field("original_lease_id", &self.original.lease_id())
            .finish()
    }
}
impl PhysicalStoppedDispatchAuthorization {
    fn original(&self) -> ControlStoppedRun<'_> {
        ControlStoppedRun {
            agent_id: &self.control_key.agent_id,
            identity_incarnation: self.control_key.identity_incarnation,
            owner_epoch: self.owner_epoch,
            run_id: self.original.run_id(),
            runtime_lease_id: self.original.lease_id(),
            activation_id: self.original.activation_id(),
            source: &self.source,
        }
    }
    fn validate_tx(
        &self,
        tx: &Transaction<'_>,
        key: &EpisodeMemoryKey,
        invocation: &EpisodeMemoryInvocation,
    ) -> Result<()> {
        ensure!(
            self.original.is_complete(),
            "original dispatch producer has not ended"
        );
        ensure!(
            self.original.source_message_id() == self.source.id,
            "original physical source changed"
        );
        let proof = control_transition::prove_committed_control_stop_for_run_tx(
            tx,
            &self.control_key,
            &self.original(),
        )?
        .context("original canonical Stop proof missing")?;
        proof.require_same_transaction(tx)?;
        ensure!(
            proof.receipt().command_digest == self.control_digest,
            "original Stop digest changed"
        );
        proof.matches_memory_dispatch(key, invocation)
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum EndedDispatchAuthorization {
    PhysicalStopped {
        authorization: PhysicalStoppedDispatchAuthorization,
    },
    Startup {
        authorization: crate::runtime_db::startup_ended_memory::StartupEndedDispatchAuthorization,
    },
}
impl EndedDispatchAuthorization {
    pub(crate) fn from_startup(
        authorization: crate::runtime_db::startup_ended_memory::StartupEndedDispatchAuthorization,
    ) -> Self {
        Self::Startup { authorization }
    }
    pub(crate) fn proof_material_tx(&self, tx: &Transaction<'_>) -> Result<(usize, usize)> {
        match self {
            Self::PhysicalStopped { authorization } => {
                control_transition::stopped_run_proof_material_tx(
                    tx,
                    &crate::runtime::resident_input_evidence::InputEvidenceScope {
                        agent_id: authorization.control_key.agent_id.clone(),
                        identity_incarnation: authorization.control_key.identity_incarnation,
                    },
                    authorization.original.run_id(),
                    authorization.original.activation_id(),
                )
            }
            Self::Startup { authorization } => authorization.proof_material_tx(tx),
        }
    }

    pub(crate) fn retired_physical_stop_address_tx(
        &self,
        tx: &Transaction<'_>,
        binding: &DispatchMemoryBinding,
    ) -> Result<crate::runtime_db::dispatch_memory::RetiredPhysicalStopAddress> {
        let Self::PhysicalStopped { authorization } = self else {
            anyhow::bail!("only the original physical lease may hand off retired memory");
        };
        authorization.validate_tx(tx, binding.key(), binding.invocation())?;
        let proof = control_transition::prove_committed_control_stop_for_run_tx(
            tx,
            &authorization.control_key,
            &authorization.original(),
        )?
        .context("original retired Stop proof missing")?;
        Ok(
            crate::runtime_db::dispatch_memory::RetiredPhysicalStopAddress {
                control_key: authorization.control_key.clone(),
                control_digest: authorization.control_digest.clone(),
                obligation_id: proof.obligation().target.obligation_id.clone(),
                owner_epoch: authorization.owner_epoch,
                run_id: authorization.original.run_id().to_owned(),
                runtime_lease_id: authorization.original.lease_id().to_owned(),
                activation_id: authorization.original.activation_id().map(str::to_owned),
                source_message_id: proof.source_message_id().to_owned(),
                source_sequence: proof.source_sequence(),
            },
        )
    }

    pub(crate) fn validate_tx(
        &self,
        tx: &Transaction<'_>,
        binding: &DispatchMemoryBinding,
    ) -> Result<()> {
        match self {
            Self::PhysicalStopped { authorization } => {
                authorization.validate_tx(tx, binding.key(), binding.invocation())
            }
            Self::Startup { authorization } => {
                authorization.validate_tx(tx, binding.key(), binding.invocation())
            }
        }
    }
}

impl RuntimeHandle {
    fn observe_original_stopped_material_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &super::resident_input_evidence::InputEvidenceScope,
        original: &ControlRunLease,
    ) -> Result<Arc<super::input_evidence_budget::BudgetViewGuard>> {
        let (mut bytes, mut records) = control_transition::stopped_run_proof_material_tx(
            tx,
            scope,
            original.run_id(),
            original.activation_id(),
        )?;
        // The small immutable Start header and its original descriptor coexist
        // with the selected canonical proof. No baseline/member body is loaded.
        let (header_bytes, header_count):(usize,usize)=tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(json_object('key',json_extract(c.payload_json,'$.start.key'),'invocation',json_extract(c.payload_json,'$.start.invocation'),'source_sequence',json_extract(c.payload_json,'$.start.source_sequence'),'source_kind',json_extract(c.payload_json,'$.start.source_kind'),'digest',json_extract(c.payload_json,'$.start.digest'),'original_start',json_extract(c.payload_json,'$.original_start')) AS BLOB))+COALESCE(length(CAST(r.prepared_json AS BLOB)),0)),0),COUNT(*) FROM dispatch_memory_checkpoints c LEFT JOIN input_mutation_receipts r ON r.command_nonce=json_extract(c.payload_json,'$.original_start.command_nonce') WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.source_message_id=?3",
            rusqlite::params![scope.agent_id,scope.identity_incarnation,original.source_message_id()],
            |row|Ok((row.get(0)?,row.get(1)?)))?;
        bytes = bytes
            .checked_add(header_bytes)
            .context("ended proof byte overflow")?;
        records = records
            .checked_add(header_count.saturating_mul(2))
            .context("ended proof count overflow")?;
        let payload = bytes.checked_mul(12).context("ended proof byte overflow")?;
        let copies = records
            .checked_mul(12)
            .context("ended proof count overflow")?;
        let metadata = copies
            .checked_mul(256)
            .context("ended proof metadata overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("ended proof budget overflow")?;
        ensure!(
            payload
                .checked_add(metadata)
                .is_some_and(|total| total as u64 <= byte_limit)
                && copies as u64 <= limits.record_limit,
            "ended original proof exceeds preparation budget"
        );
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            scope,
            super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: payload as u64,
                metadata_estimate_bytes: metadata as u64,
            },
        )?)
    }

    async fn finish_retired_stopped_dispatch(
        &self,
        original: &Arc<ControlRunLease>,
    ) -> Result<bool> {
        use crate::runtime_db::dispatch_memory::{
            load_retired_dispatch_memory_binding_tx, prepare_retired_physical_handoff_tx,
            retired_physical_handoff_exists_tx,
        };
        use rusqlite::OptionalExtension;
        let mut reservations = super::mutation_owner::MutationOwner::reserve_all(vec![self
            .inner
            .mutation_owner
            .clone()])
        .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        let scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let owner_epoch = self.inner.mutation_owner.owner_epoch();
        self.inner.runtime_db.transaction(|tx| {
            let identity: Option<(u64, String, String, String, String)> = tx
                .query_row(
                    "SELECT json_extract(payload_json,'$.incarnation'),status,agent_id,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.status') FROM agent_identities WHERE agent_id=?1",
                    [&scope.agent_id],
                    |row| Ok((row.get(0)?, row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
                )
                .optional()?;
            if let Some((inc,status,agent,payload_agent,payload_status))=&identity {
                ensure!(*inc>=scope.identity_incarnation && agent==&scope.agent_id
                    && payload_agent==agent && payload_status==status,
                    "ended handoff canonical identity address disagrees");
            }
            if identity.as_ref().is_some_and(|(inc, status, ..)| {
                *inc == scope.identity_incarnation
                    && matches!(status.as_str(), "active" | "deleting")
            }) {
                return Ok(false);
            }
            let unfinished: bool=tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND source_message_id=?3 AND state!='done')",
                rusqlite::params![scope.agent_id,scope.identity_incarnation,original.source_message_id()],|row|row.get(0))?;
            if !unfinished { return Ok(true); }
            if retired_physical_handoff_exists_tx(
                tx,
                &scope,
                original.run_id(),
                original.lease_id(),
                owner_epoch,
                original.source_message_id(),
            )? {
                return Ok(true);
            }
            let _budget = self.observe_original_stopped_material_tx(tx, &scope, original)?;
            let source = ContextDurableRead::new(tx, &scope.agent_id)
                .message_by_id(original.source_message_id())?
                .context("retired original source missing before handoff")?;
            let run = ControlStoppedRun {
                agent_id: &scope.agent_id,
                identity_incarnation: scope.identity_incarnation,
                owner_epoch,
                run_id: original.run_id(),
                runtime_lease_id: original.lease_id(),
                activation_id: original.activation_id(),
                source: &source,
            };
            let proof = control_transition::find_committed_control_stop_for_run_tx(tx, &run)?
                .context("retired unfinished memory has no original canonical Stop")?;
            let address = proof.dispatch().context("retired unfinished memory has no actual Stop Turn")?;
            let Some(binding) =
                load_retired_dispatch_memory_binding_tx(tx, &scope, address.turn_id())?
            else {
                return Ok(true);
            };
            let authorization = EndedDispatchAuthorization::PhysicalStopped {
                authorization: PhysicalStoppedDispatchAuthorization {
                    control_key: proof.receipt().key.clone(),
                    control_digest: proof.receipt().command_digest.clone(),
                    owner_epoch,
                    source: Box::new(source),
                    original: original.clone(),
                },
            };
            authorization.validate_tx(tx, &binding)?;
            prepare_retired_physical_handoff_tx(
                tx,
                &scope,
                address.turn_id(),
                &authorization,
                self.now(),
            )?;
            // No AgentState/domain/publication changes. The original Control Run
            // obligation remains the durable retry address until this SQL ACK.
            Ok(true)
        })
    }

    /// Called only after the original scheduled guard dropped or its exact
    /// Control completion worker joined. A cleared State is never this proof.
    pub(super) fn finish_original_stopped_dispatch<'a>(
        &'a self,
        original: Arc<ControlRunLease>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            ensure!(
                original.is_complete(),
                "stopped dispatch still has its original producer"
            );
            if !original.was_stopped() {
                return Ok(());
            }
            if self.finish_retired_stopped_dispatch(&original).await? {
                return Ok(());
            }
            let mut mutation = match self.begin_mutation().await {
                Ok(mutation) => mutation,
                Err(error)
                    if error
                        .downcast_ref::<super::state_mutation::RuntimeMutationIdentityRetired>()
                        .is_some() =>
                {
                    ensure!(
                        self.finish_retired_stopped_dispatch(&original).await?,
                        "original identity retirement changed during ended handoff"
                    );
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            let scope = mutation.input_evidence_scope();
            let owner_epoch = self.inner.mutation_owner.owner_epoch();
            let state = mutation.last_persisted_state.clone();
            // The ordinary completed path has no Stop work. Do not discover a
            // different command while its original run is still canonical.
            if state.current_run_id.as_deref() == Some(original.run_id()) {
                return Ok(());
            }
            let selected = self.inner.runtime_db.transaction(|tx| {
                let unfinished: bool=tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND source_message_id=?3 AND state!='done')",
                    rusqlite::params![scope.agent_id,scope.identity_incarnation,original.source_message_id()],|row|row.get(0))?;
                if !unfinished { return Ok(None); }
                let _source_budget =
                    self.observe_original_stopped_material_tx(tx, &scope, &original)?;
                let source = ContextDurableRead::new(tx, &scope.agent_id)
                    .message_by_id(original.source_message_id())?
                    .context("original physical dispatch source missing")?;
                let run = ControlStoppedRun {
                    agent_id: &scope.agent_id,
                    identity_incarnation: scope.identity_incarnation,
                    owner_epoch,
                    run_id: original.run_id(),
                    runtime_lease_id: original.lease_id(),
                    activation_id: original.activation_id(),
                    source: &source,
                };
                let Some(proof) =
                    control_transition::find_committed_control_stop_for_run_tx(tx, &run)?
                else {
                    return Ok(None);
                };
                proof.require_same_transaction(tx)?;
                let Some(address) = proof.dispatch() else {
                    return Ok(None);
                };
                let Some(binding) =
                    load_dispatch_memory_binding_by_turn_tx(tx, &scope, address.turn_id())?
                else {
                    return Ok(None);
                };
                proof.matches_memory_dispatch(binding.key(), binding.invocation())?;
                let turn = ContextDurableRead::new(tx, &scope.agent_id)
                    .turn_by_id(address.turn_id())?
                    .context("ended original Turn missing")?;
                let authorization = EndedDispatchAuthorization::PhysicalStopped {
                    authorization: PhysicalStoppedDispatchAuthorization {
                        control_key: proof.receipt().key.clone(),
                        control_digest: proof.receipt().command_digest.clone(),
                        owner_epoch,
                        source: Box::new(source),
                        original: original.clone(),
                    },
                };
                authorization.validate_tx(tx, &binding)?;
                Ok(Some((binding, turn, authorization, _source_budget)))
            })?;
            let Some((binding, turn, authorization, _source_budget)) = selected else {
                return Ok(());
            };
            let mut request = super::resident_work::WorkCaptureRequest::current();
            request.required_turn_ids.insert(turn.turn_id.clone());
            request
                .required_trigger_message_ids
                .insert(binding.invocation().message_id.clone());
            mutation.prepare_owner_work_request(&request).await?;
            let frozen = crate::memory::episode_transition::FrozenEpisodeMemoryValues {
                timestamp: self.now(),
                initial_builder_id: crate::ids::episode_id(),
                fallback_builder_id: crate::ids::episode_id(),
                next_builder_id: crate::ids::episode_id(),
                audit_event_id: crate::ids::audit_event_id(),
            };
            let prepared = PreparedInputTransition::terminal(
                scope,
                crate::ids::audit_event_id(),
                self.now(),
                TurnTerminalTransitionCommand {
                    agent_id: state.id.clone(),
                    agent_state: AgentStateMutation {
                        expected: Some(Box::new(state.clone())),
                        record: Box::new(state),
                    },
                    turn_record: turn,
                    terminal_tool_executions: Vec::new(),
                    audit_events: Vec::new(),
                    fault: self.take_transition_fault(),
                },
            )?
            .with_ended_dispatch_memory(
                DispatchMemoryIntent::Finish { binding, frozen },
                authorization,
            )?;
            let commit = mutation.commit_prepared_input_transition(prepared).await?;
            drop(mutation);
            self.apply_transition_commit(commit).await;
            Ok(())
        })
    }
}

#[cfg(test)]
pub(crate) fn test_physical_stopped_authorization(
    control_key: ControlKey,
    control_digest: String,
    owner_epoch: u64,
    source: MessageEnvelope,
    run_id: &str,
    lease_id: &str,
    activation_id: Option<String>,
    completed: bool,
) -> EndedDispatchAuthorization {
    let original =
        ControlRunLease::test_stopped_run(run_id, lease_id, activation_id, &source.id, completed);
    EndedDispatchAuthorization::PhysicalStopped {
        authorization: PhysicalStoppedDispatchAuthorization {
            control_key,
            control_digest,
            owner_epoch,
            source: Box::new(source),
            original,
        },
    }
}
