//! Fresh finite original Stop observation; no Runtime or new Host admission.
use super::*;
use crate::{
    runtime::input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    runtime_db::{
        task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError},
        task_stop::{load_task_stop_by_sequence_tx, TaskStopRecord},
    },
};

/// Construction requires actual original-row/whole-receipt validation under its
/// scope owner. It grants only this Stop's existing continuation address.
pub(crate) struct OriginalTaskStopMetadataRead {
    pub(crate) original: Arc<TaskStopRecord>,
    _source: Arc<BudgetViewGuard>,
    _owner_state: crate::runtime_db::task_stop::OriginalTaskStopOwnerState,
}
impl OriginalTaskStopMetadataRead {
    /// Availability only; false promises neither prepared Task nor Ready.
    pub(crate) fn owner_state_unavailable(&self) -> bool {
        self._owner_state.owner_state_unavailable()
    }
    pub(crate) fn record(&self) -> &TaskStopRecord {
        &self.original
    }
    pub(crate) fn shared_record(&self) -> Arc<TaskStopRecord> {
        self.original.clone()
    }
}
/// Keeps the current owner and all selector/source accounting alive through a
/// synchronous original-ACK publication check. No root is retained here.
struct HeldOriginalTaskStopMetadataRead {
    reservation: MutationReservation,
    read: OriginalTaskStopMetadataRead,
    _selectors: Arc<BudgetViewGuard>,
}
impl RuntimeHostBridge {
    /// The opaque local capability proves the original SQL invocation ended.
    /// Current owner reconciliation may resolve its exact witness/group, but
    /// this read never hydrates or installs through the old Runtime epoch.
    pub(crate) async fn read_finished_original_task_stop_attempt(
        &self,
        finished: &crate::runtime::FinishedOriginalTaskStopAttempt,
        request: &TaskDomainLoadRequest,
    ) -> Result<Option<OriginalTaskStopMetadataRead>> {
        use rusqlite::OptionalExtension;
        let prepared = finished.prepared();
        let intent = finished.intent();
        let scope = &intent.task.scope;
        let host = self.host()?;
        let ledger = &host.inner.input_evidence_budget;
        let bytes = serialized_bytes(&(scope, prepared.command_nonce(), prepared.scopes()))?
            .checked_mul(8)
            .and_then(|n| n.checked_add(1024))
            .context("finished original Stop selector overflow")?;
        anyhow::ensure!(
            bytes <= u64::try_from(request.byte_limit.min(64 * 1024 * 1024))?
                && prepared
                    .scopes()
                    .len()
                    .checked_mul(8)
                    .is_some_and(|n| n <= request.record_limit.min(65_536))
                && request.record_limit.min(65_536) >= 8,
            TaskSnapshotError::ResourceExhausted("finished original Stop selectors")
        );
        let _selectors = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let reservations = host
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        anyhow::ensure!(
            reservations.len() == 1,
            "finished original Stop owner group changed"
        );
        let observed=host.runtime_db().transaction(|tx|{
            // Integer-only lookup also rejects a conflicting original nonce
            // moved to a foreign normalized scope; it is never joint absence.
            let selected:Option<(u64,bool)>=tx.query_row(
                "SELECT effect_seq,COALESCE(agent_id=?2 AND identity_incarnation=?3,0) FROM task_stop_effects WHERE command_nonce=?1",
                rusqlite::params![prepared.command_nonce(),scope.agent_id,scope.identity_incarnation],
                |r|Ok((r.get(0)?,r.get(1)?)),
            ).optional()?;
            let Some((seq,matching_scope))=selected else {
                let receipt:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_mutation_receipts WHERE command_nonce=?1)",
                    [prepared.command_nonce()],|r|r.get(0))?;
                anyhow::ensure!(!receipt,"original TaskStop whole receipt lost its mandatory effect");
                return Ok(None);
            };
            anyhow::ensure!(matching_scope,"finished original Stop normalized scope differs");
            let owner_state=crate::runtime_db::task_stop::observe_original_task_stop_owner_state_tx(tx,scope,request,ledger)?;
            let selected=load_task_stop_by_sequence_tx(tx,scope,seq,request.byte_limit,ledger)?;
            Ok(Some((selected,owner_state)))
        })?;
        let Some((selected, owner_state)) = observed else {
            return Ok(None);
        };
        let original = selected
            .records
            .first()
            .context("finished original Stop record missing")?;
        anyhow::ensure!(
            original.prepared == *prepared && original.intent == *intent,
            "finished original Stop immutable source changed"
        );
        let source = ledger.register_task_stop_view(
            scope,
            InputMemoryCategory::Candidate,
            std::iter::once(original),
        )?;
        let result = OriginalTaskStopMetadataRead {
            original: original.clone(),
            _source: source,
            _owner_state: owner_state,
        };
        drop(reservations);
        Ok(Some(result))
    }
    pub(crate) async fn read_original_task_stop_metadata(
        &self,
        expected: &TaskStopRecord,
        request: &TaskDomainLoadRequest,
    ) -> Result<OriginalTaskStopMetadataRead> {
        Ok(self
            .hold_original_task_stop_metadata(expected, request)
            .await?
            .read)
    }

    /// The callback is synchronous: the actual current reservation remains held
    /// while the caller compares publication with this fresh exact whole ACK.
    pub(crate) async fn with_original_task_stop_metadata<R>(
        &self,
        expected: &TaskStopRecord,
        request: &TaskDomainLoadRequest,
        read: impl FnOnce(&MutationReservation, &OriginalTaskStopMetadataRead) -> Result<R>,
    ) -> Result<R> {
        let held = self
            .hold_original_task_stop_metadata(expected, request)
            .await?;
        read(&held.reservation, &held.read)
    }

    /// Preparation uses only an actual Runtime registered for the current
    /// reservation. A cold or retired original never constructs a Runtime.
    pub(crate) async fn prepare_original_task_stop_publication(
        &self,
        expected: &TaskStopRecord,
        request: &TaskDomainLoadRequest,
    ) -> Result<()> {
        let held = self
            .hold_original_task_stop_metadata(expected, request)
            .await?;
        anyhow::ensure!(
            !held.read.owner_state_unavailable(),
            "original Stop owner State unavailable for publication"
        );
        let current = self
            .runtime_for_mutation(&held.reservation)
            .await?
            .context("original Stop has no current Runtime for publication preparation")?;
        let original = held.read;
        // The current Runtime's named actor acquires its own owner. Keep only
        // the exact original body/guard across that await; never a reservation.
        drop(held.reservation);
        drop(held._selectors);
        current
            .prepare_current_task_stop_publication(original.record(), request)
            .await
    }

    async fn hold_original_task_stop_metadata(
        &self,
        expected: &TaskStopRecord,
        request: &TaskDomainLoadRequest,
    ) -> Result<HeldOriginalTaskStopMetadataRead> {
        let host = self.host()?;
        let ledger = &host.inner.input_evidence_budget;
        let scope = &expected.intent.task.scope;
        let bytes = serialized_bytes(&(scope, expected.effect_seq))?
            .checked_mul(8)
            .and_then(|n| n.checked_add(1024))
            .context("original Stop selector overflow")?;
        anyhow::ensure!(
            bytes <= u64::try_from(request.byte_limit.min(64 * 1024 * 1024))?
                && request.record_limit.min(65_536) >= 8,
            TaskSnapshotError::ResourceExhausted("original Stop selector")
        );
        let selectors = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let mut reservations = host
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        anyhow::ensure!(reservations.len() == 1, "original Stop owner group changed");
        let (selected, owner_state) = host.runtime_db().transaction(|tx| {
            // The existing identity observer parses JSON scalar predicates only
            // after actual length/request/Host ledger admission.
            let owner_state =
                crate::runtime_db::task_stop::observe_original_task_stop_owner_state_tx(
                    tx, scope, request, ledger,
                )?;
            let selected = load_task_stop_by_sequence_tx(
                tx,
                scope,
                expected.effect_seq,
                request.byte_limit,
                ledger,
            )?;
            Ok((selected, owner_state))
        })?;
        let original = selected
            .records
            .first()
            .context("original Stop record missing")?;
        anyhow::ensure!(
            original.effect_seq == expected.effect_seq
                && original.prepared == expected.prepared
                && original.intent.task.scope == *scope
                && original.intent_digest == expected.intent_digest
                && original.generation_digest == expected.generation_digest
                && original.ack_task_digest == expected.ack_task_digest,
            "original Stop immutable address changed"
        );
        let source = ledger.register_task_stop_view(
            scope,
            InputMemoryCategory::Candidate,
            std::iter::once(original),
        )?;
        let result = OriginalTaskStopMetadataRead {
            original: original.clone(),
            _source: source,
            _owner_state: owner_state,
        };
        let reservation = reservations.pop().context("original Stop owner missing")?;
        Ok(HeldOriginalTaskStopMetadataRead {
            reservation,
            read: result,
            _selectors: selectors,
        })
    }

    /// Only an already-owned original Stop continuation may use this path.
    /// Its sealed read plus the kernel's second exact tuple/identity checks are
    /// independent of Open admission and remain valid during Closing.
    pub(crate) async fn stop_private_agent_for_owned_task_stop(
        &self,
        original: &OriginalTaskStopMetadataRead,
    ) -> Result<TaskStopChildRetirement> {
        self.host()?
            .stop_private_agent_for_owned_task_stop_record(original.shared_record())
            .await
    }
}
impl RuntimeHost {
    /// Shared by Open's registered wrapper and the already-owned Closing worker.
    /// The original kernel performs all identity/tombstone/drain/cleanup ACKs.
    pub(super) async fn stop_private_agent_for_owned_task_stop_record(
        &self,
        original: Arc<TaskStopRecord>,
    ) -> Result<TaskStopChildRetirement> {
        let target = original
            .intent
            .child
            .as_ref()
            .context("TaskStop original child unavailable")?;
        let (evidence, complete_producers) = self
            .archive_private_agent_owned_with_original(&target.scope.agent_id, Some(&original))
            .await?;
        let evidence = evidence.context("TaskStop child cleanup lost original identity proof")?;
        let mut retired = TaskStopChildRetirement {
            original,
            _identity: evidence,
            producer_exit: None,
        };
        if let Some(complete) = complete_producers {
            retired.producer_exit =
                TaskStopWorktreeProducerExit::from_complete_original_child_drain(
                    &retired,
                    complete,
                    &self.inner.input_evidence_budget,
                )?
                .map(Arc::new);
        }
        Ok(retired)
    }
}
