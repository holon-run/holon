//! Closed Timer commands preserve their original merge/new-wake branch.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_scheduling::{
        CanonicalSchedulingFact, SchedulingCaptureRequest, SchedulingFamily, SchedulingKey,
        SchedulingWakeStatus,
    },
};
use crate::runtime_db::{
    scheduling_snapshot as scheduling, TimerCancel, TimerFire, TimerWakeRecord,
};
use std::sync::Arc;

#[derive(Debug, serde::Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum PreparedTimerTransition {
    Create {
        record: crate::types::TimerRecord,
        #[serde(skip)]
        fault: Option<TransitionFaultPoint>,
    },
    Fire {
        command: Box<TimerFire>,
        pending: Option<TimerWakeRecord>,
        #[serde(skip)]
        fault: Option<TransitionFaultPoint>,
    },
    Cancel {
        command: Box<TimerCancel>,
        pending: Option<TimerWakeRecord>,
        #[serde(skip)]
        fault: Option<TransitionFaultPoint>,
    },
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TimerMutationOutcome {
    Created {
        record: crate::types::TimerRecord,
    },
    Fired {
        result: crate::runtime_db::TimerFireResult,
    },
    Cancelled {
        result: crate::runtime_db::TimerCancelResult,
    },
}
impl PreparedTimerTransition {
    pub(crate) fn create(
        record: crate::types::TimerRecord,
        fault: Option<TransitionFaultPoint>,
    ) -> Self {
        Self::Create { record, fault }
    }
    pub(crate) fn fire(
        command: TimerFire,
        pending: Option<TimerWakeRecord>,
        fault: Option<TransitionFaultPoint>,
    ) -> Self {
        Self::Fire {
            command: Box::new(command),
            pending,
            fault,
        }
    }
    pub(crate) fn cancel(
        command: TimerCancel,
        pending: Option<TimerWakeRecord>,
        fault: Option<TransitionFaultPoint>,
    ) -> Self {
        Self::Cancel {
            command: Box::new(command),
            pending,
            fault,
        }
    }
    pub(super) fn agent_id(&self) -> &str {
        &self.record().agent_id
    }
    fn record(&self) -> &crate::types::TimerRecord {
        match self {
            Self::Create { record, .. } => record,
            Self::Fire { command, .. } => &command.record,
            Self::Cancel { command, .. } => &command.record,
        }
    }
    pub(super) fn new_wake(&self) -> Option<(&MessageEnvelope, &QueueEntryRecord)> {
        match self {
            Self::Fire {
                command,
                pending: None,
                ..
            } => Some((&command.message, &command.queue_entry)),
            _ => None,
        }
    }
    pub(super) fn validate(&self, scope: &InputEvidenceScope) -> Result<()> {
        anyhow::ensure!(
            self.agent_id() == scope.agent_id && !self.record().id.is_empty(),
            "Timer command namespace differs"
        );
        match self {
            Self::Create { record, .. } => anyhow::ensure!(
                record.status == crate::types::TimerStatus::Active && record.fire_count == 0,
                "Timer creation is not a fresh Active timer"
            ),
            Self::Fire {
                command, pending, ..
            } => {
                anyhow::ensure!(
                    command.expected.agent_id == scope.agent_id
                        && command.expected.id == command.record.id
                        && command.message.agent_id == scope.agent_id
                        && command.queue_entry.agent_id == scope.agent_id
                        && command.queue_entry.message_id == command.message.id
                        && command.agent_state.0.id == scope.agent_id
                        && command.agent_state.1.id == scope.agent_id,
                    "Timer fire targets differ from primary scope"
                );
                anyhow::ensure!(
                    matches!(&command.message.origin,crate::types::MessageOrigin::Timer {timer_id} if timer_id==&command.record.id)
                        && command.message.kind == crate::types::MessageKind::TimerTick
                        && command.queue_entry.status == QueueEntryStatus::Queued,
                    "Timer fire has another wake source"
                );
                validate_dispatch_preserved(&command.agent_state.0, &command.agent_state.1)?;
                validate_pending(pending, &command.record.id)?;
            }
            Self::Cancel {
                command, pending, ..
            } => {
                anyhow::ensure!(
                    command.expected.agent_id == scope.agent_id
                        && command.expected.id == command.record.id
                        && command
                            .agent_state
                            .as_ref()
                            .is_none_or(|(a, b)| a.id == scope.agent_id && b.id == scope.agent_id),
                    "Timer cancellation namespace differs"
                );
                if let Some((expected, record)) = &command.agent_state {
                    validate_dispatch_preserved(expected, record)?;
                }
                validate_pending(pending, &command.record.id)?;
                anyhow::ensure!(
                    command.agent_state.is_some() == pending.is_some(),
                    "Timer cancellation State differs from frozen pending branch"
                );
            }
        }
        Ok(())
    }
    pub(super) fn execute_tx(
        &self,
        tx: &Transaction<'_>,
        db: &RuntimeDb,
        scope: &InputEvidenceScope,
        work: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        tasks: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<(TransitionCommit, InputMutationResult)> {
        let mut value = TransitionCommit::default();
        let outcome = match self {
            Self::Create { record, fault } => {
                inject_fault(*fault, TransitionFaultPoint::AfterValidation)?;
                repositories::upsert_timer_tx(tx, record)?;
                value.applied = true;
                value.effects.fault = *fault;
                value
                    .effects
                    .scheduling_write_keys
                    .insert(SchedulingKey::new(SchedulingFamily::Timer, &record.id));
                let actual = repositories::timer_tx(tx, &record.id)?
                    .context("Timer creation ACK missing")?;
                value.effects.timers.push(actual.clone());
                inject_fault(*fault, TransitionFaultPoint::AfterCanonicalWrites)?;
                TimerMutationOutcome::Created { record: actual }
            }
            Self::Fire {
                command,
                pending,
                fault,
            } => {
                let ack = db.timers().fire_with_wait_tx(
                    tx,
                    command,
                    *fault,
                    Some((scope, work, tasks, ledger)),
                    true,
                    &mut value.effects.wait_condition_writes,
                    &mut value.effects.audit_events,
                    memory,
                )?;
                value.applied = ack.record.advanced;
                value.effects.input_evidence_delta = ack.input_delta;
                value.effects.work_delta = ack.work_delta;
                value.effects.task_delta = ack.task_delta;
                value.effects.task_delta_requires_prepared_domain = true;
                value.effects.task_material_budget_views = ack.task_material_budget_views;
                value.effects.agent_state = ack.agent_state;
                if ack.record.advanced {
                    value.effects.fault = *fault;
                    value.effects.timers.push(
                        repositories::timer_tx(tx, &command.record.id)?
                            .context("Timer fire ACK missing")?,
                    );
                    value
                        .effects
                        .scheduling_write_keys
                        .insert(SchedulingKey::new(
                            SchedulingFamily::Timer,
                            &command.record.id,
                        ));
                }
                if ack.record.wake_created {
                    anyhow::ensure!(pending.is_none(), "Timer merge unexpectedly created a wake");
                    value
                        .effects
                        .scheduling_write_keys
                        .insert(SchedulingKey::wake(&command.record.id, &command.message.id));
                    let message = ack
                        .record
                        .message
                        .as_ref()
                        .context("Timer wake ACK has no Message")?;
                    value.effects.messages.push(message.clone());
                    value.effects.queued_messages.push(message.clone());
                    value.effects.notify_scheduler = true;
                }
                TimerMutationOutcome::Fired { result: ack.record }
            }
            Self::Cancel {
                command,
                pending,
                fault,
            } => {
                inject_fault(*fault, TransitionFaultPoint::AfterValidation)?;
                let result = db.timers().cancel_tx(tx, command)?;
                value.applied = result.cancelled;
                if result.cancelled {
                    value.effects.fault = *fault;
                    value
                        .effects
                        .scheduling_write_keys
                        .insert(SchedulingKey::new(
                            SchedulingFamily::Timer,
                            &command.record.id,
                        ));
                    if let Some(wake) = pending {
                        value
                            .effects
                            .scheduling_write_keys
                            .insert(SchedulingKey::wake(&wake.timer_id, &wake.message_id));
                    }
                    value.effects.timers.push(
                        repositories::timer_tx(tx, &command.record.id)?
                            .context("Timer cancel ACK missing")?,
                    );
                    value
                        .effects
                        .removed_queue_message_ids
                        .extend(result.dropped_message_id.iter().cloned());
                    if let Some((expected, _)) = &command.agent_state {
                        let actual = crate::runtime_db::context_inputs::ContextDurableRead::new(
                            tx,
                            &scope.agent_id,
                        )
                        .scope_facts()?
                        .state
                        .context("Timer cancel State ACK missing")?;
                        value.effects.agent_state = Some(AgentStateMutation {
                            expected: Some(Box::new(expected.clone())),
                            record: Box::new(actual),
                        });
                    }
                }
                inject_fault(*fault, TransitionFaultPoint::AfterCanonicalWrites)?;
                TimerMutationOutcome::Cancelled { result }
            }
        };
        Ok((
            value,
            InputMutationResult::Timer {
                outcome: Box::new(outcome),
            },
        ))
    }
    pub(super) fn fault(&self) -> Option<TransitionFaultPoint> {
        match self {
            Self::Create { fault, .. } | Self::Fire { fault, .. } | Self::Cancel { fault, .. } => {
                *fault
            }
        }
    }
}
fn validate_dispatch_preserved(expected: &AgentState, record: &AgentState) -> Result<()> {
    anyhow::ensure!(
        record.current_run_id == expected.current_run_id
            && record.current_turn_id == expected.current_turn_id
            && record.current_execution_binding == expected.current_execution_binding
            && record.turn_index == expected.turn_index,
        "Timer cannot start/change an actual dispatch binding"
    );
    Ok(())
}
fn validate_pending(pending: &Option<TimerWakeRecord>, timer: &str) -> Result<()> {
    anyhow::ensure!(
        pending.as_ref().is_none_or(|wake| wake.timer_id == timer
            && wake.status == crate::runtime_db::TimerWakeStatus::Pending
            && !wake.message_id.is_empty()),
        "invalid frozen pending wake"
    );
    Ok(())
}
pub(super) struct SchedulingTransaction<'tx, 'conn> {
    proof: scheduling::SchedulingWriteProof<'tx, 'conn>,
    fire_wake: bool,
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl SchedulingTransaction<'_, '_> {
    pub(super) fn fires_wake(&self) -> bool {
        self.fire_wake
    }
    pub(super) fn finish(
        self,
        tx: &Transaction<'_>,
        value: &mut TransitionCommit,
        request: &scheduling::SchedulingDomainLoadRequest,
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<()> {
        let state = crate::runtime_db::context_inputs::ContextDurableRead::new(
            tx,
            &self.proof.scope().agent_id,
        )
        .scope_facts()?
        .state
        .context("Scheduling finalizer has no final State")?;
        let mut facts = scheduling::register_scheduling_writes_with_memory_budget_tx(
            tx,
            &self.proof,
            &value.effects.scheduling_write_keys,
            &state,
            request,
            memory,
        )?;
        value
            .effects
            .scheduling_material_budget_views
            .extend(self.guards);
        value
            .effects
            .scheduling_material_budget_views
            .extend(facts.take_budget_views());
        value.effects.scheduling_delta_requires_prepared_domain = facts.requires_prepared_domain;
        value.effects.scheduling_delta = Some(Box::new(facts.delta));
        Ok(())
    }
}
pub(super) fn prepare_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    prepared: &PreparedInputTransition,
    scope: &InputEvidenceScope,
    request: &scheduling::SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    mut memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<Option<SchedulingTransaction<'tx, 'conn>>> {
    let (timer, queue_claim) = match prepared.command.as_ref() {
        InputTransitionCommand::Timer(timer) => (Some(timer), None),
        InputTransitionCommand::Queue(queue) => match queue.timer_wake_claim.as_ref() {
            Some(claim) => (None, Some(claim)),
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    let id = timer
        .map(|timer| timer.record().id.as_str())
        .or_else(|| queue_claim.map(|claim| claim.timer_id.as_str()))
        .expect("closed Timer operation");
    let timer_key = SchedulingKey::new(SchedulingFamily::Timer, id);
    let mut keys = BTreeSet::from([timer_key.clone()]);
    let mut proof = scheduling::prepare_scheduling_write_with_memory_budget_tx(
        tx,
        scope,
        &keys,
        request,
        ledger,
        memory.as_deref_mut(),
    )?;
    if let Some(timer) = timer {
        let bytes = usize::try_from(serialized_bytes(timer)?)?
            .checked_mul(8)
            .context("Timer intent material overflow")?;
        let remaining = proof.remaining_request(request)?;
        anyhow::ensure!(
            bytes <= remaining.byte_limit && remaining.record_limit >= 8,
            scheduling::SchedulingSnapshotError::ResourceExhausted("Timer intent material")
        );
        if let Some(memory) = memory.as_deref_mut() {
            memory.additional_material(bytes, 8, 1)?;
        }
        proof.absorb_material(bytes, 8, request)?;
    }
    let mut fire_wake = false;
    let mut guards = Vec::new();
    if let Some(timer) = timer {
        guards.push(
            ledger.register_owned_bytes(
                scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: serialized_bytes(timer)?
                        .checked_mul(8)
                        .context("Timer guard overflow")?,
                    metadata_estimate_bytes: 0,
                },
            )?,
        );
    }
    if let Some(timer) = timer {
        timer.validate(scope)?;
        match timer {
            PreparedTimerTransition::Create { .. } => anyhow::ensure!(
                proof.fact_before(&timer_key).is_none(),
                "Timer creation ID already exists"
            ),
            PreparedTimerTransition::Fire {
                command, pending, ..
            } => {
                if matches!(proof.fact_before(&timer_key),Some(CanonicalSchedulingFact::Timer(actual)) if actual==&command.expected&&actual.status==crate::types::TimerStatus::Active)
                {
                    verify_pending_presence(tx, id, pending)?;
                    fire_wake = pending.is_none();
                    let message = pending
                        .as_ref()
                        .map_or(command.message.id.as_str(), |w| w.message_id.as_str());
                    keys.insert(SchedulingKey::wake(id, message));
                    if fire_wake {
                        let remaining = proof.remaining_request(request)?;
                        let task_material =
                            crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                                query: Default::default(),
                                capture: crate::runtime::resident_task::TaskCaptureRequest::current(
                                ),
                                byte_limit: remaining.byte_limit,
                                record_limit: remaining.record_limit,
                            };
                        let (message_guards, bytes, records) =
                            task_writes::prepare_timer_message_material_tx(
                                tx,
                                scope,
                                &command.message,
                                &task_material,
                                ledger,
                                memory.as_deref_mut(),
                            )?;
                        proof.absorb_material(bytes, records, request)?;
                        guards.extend(message_guards);
                        let remaining = proof.remaining_request(request)?;
                        let (bytes, records) = charge_queue(
                            tx,
                            scope,
                            &command.queue_entry.message_id,
                            &remaining,
                            ledger,
                            memory.as_deref_mut(),
                            &mut guards,
                        )?;
                        proof.absorb_material(bytes, records, request)?;
                    }
                }
            }
            PreparedTimerTransition::Cancel {
                command, pending, ..
            } => {
                if matches!(proof.fact_before(&timer_key),Some(CanonicalSchedulingFact::Timer(actual)) if actual==&command.expected&&actual.status==crate::types::TimerStatus::Active)
                {
                    verify_pending_presence(tx, id, pending)?;
                    if let Some(wake) = pending {
                        keys.insert(SchedulingKey::wake(id, &wake.message_id));
                        let remaining = proof.remaining_request(request)?;
                        let (bytes, records) = charge_queue(
                            tx,
                            scope,
                            &wake.message_id,
                            &remaining,
                            ledger,
                            memory.as_deref_mut(),
                            &mut guards,
                        )?;
                        proof.absorb_material(bytes, records, request)?;
                    }
                }
            }
        }
    }
    if let Some(claim) = queue_claim {
        keys.insert(SchedulingKey::wake(&claim.timer_id, &claim.message_id));
    }
    scheduling::reserve_scheduling_write_keys_tx(
        tx,
        &mut proof,
        &keys,
        request,
        memory.as_deref_mut(),
    )?;
    if let Some(pending) = timer.and_then(|timer| match timer {
        PreparedTimerTransition::Fire { pending, .. }
        | PreparedTimerTransition::Cancel { pending, .. } => pending.as_ref(),
        _ => None,
    }) {
        if let Some(CanonicalSchedulingFact::TimerWake(actual)) =
            proof.fact_before(&SchedulingKey::wake(id, &pending.message_id))
        {
            anyhow::ensure!(
                actual.status == SchedulingWakeStatus::Pending
                    && actual.fire_count == pending.fire_count
                    && actual.created_at == pending.created_at
                    && actual.updated_at == pending.updated_at
                    && actual.incorporated_at == pending.incorporated_at
                    && actual.cancelled_at == pending.cancelled_at,
                "frozen pending wake changed"
            );
        }
    }
    // Frozen command/selected queue guards survive through receipt and first ACK.
    Ok(Some(SchedulingTransaction {
        proof,
        fire_wake,
        guards,
    }))
}
fn verify_pending_presence(
    tx: &Transaction<'_>,
    timer: &str,
    pending: &Option<TimerWakeRecord>,
) -> Result<()> {
    let (count,exact):(usize,bool)=tx.query_row("SELECT COUNT(*),COALESCE(MAX(message_id=?2 AND fire_count=?3),0) FROM timer_wakes WHERE timer_id=?1 AND status='pending'",rusqlite::params![timer,pending.as_ref().map(|w|w.message_id.as_str()),pending.as_ref().map(|w|w.fire_count)],|r|Ok((r.get(0)?,r.get(1)?)))?;
    anyhow::ensure!(
        if pending.is_some() {
            count == 1 && exact
        } else {
            count == 0
        },
        "Timer pending branch changed before business SQL"
    );
    Ok(())
}
fn charge_queue(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    id: &str,
    request: &scheduling::SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    guards: &mut Vec<Arc<BudgetViewGuard>>,
) -> Result<(usize, usize)> {
    let bytes:usize=tx.query_row("SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+128),0) FROM queue_entries WHERE message_id=?1",[id],|r|r.get(0))?;
    let bytes = bytes
        .checked_mul(8)
        .context("Timer Queue material overflow")?;
    anyhow::ensure!(
        bytes <= request.byte_limit.min(64 * 1024 * 1024) && request.record_limit >= 8,
        scheduling::SchedulingSnapshotError::ResourceExhausted("Timer queue material")
    );
    if let Some(memory) = memory {
        memory.additional_material(bytes, 8, 1)?;
    }
    guards.push(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: bytes.try_into()?,
            metadata_estimate_bytes: 0,
        },
    )?);
    let owned: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1 AND agent_id<>?2)",
        rusqlite::params![id, scope.agent_id],
        |r| r.get(0),
    )?;
    anyhow::ensure!(owned, "Timer Queue target belongs to another scope");
    Ok((bytes, 8))
}
pub(super) fn default_request(
    work: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
) -> scheduling::SchedulingDomainLoadRequest {
    scheduling::SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        record_limit: work.record_limit,
        byte_limit: work.byte_limit,
    }
}
