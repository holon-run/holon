//! The original Stop's frozen Result continuation, never a synthetic Task no-op.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_task::{TaskFamily, TaskKey},
    resident_work::{WorkFamily, WorkKey},
};
use crate::runtime_db::{
    task_result_settlement as result, task_snapshot as tasks, task_stop as stops,
};
use std::sync::Arc;

#[derive(Debug, serde::Serialize)]
pub(super) struct PreparedTaskStopResultCommand {
    address: stops::TaskStopResultAddress,
    plan: stops::TaskStopResultPlan,
    #[serde(skip)]
    ready_runtime_only: bool,
    #[serde(skip)]
    fault: Option<TransitionFaultPoint>,
    #[serde(skip)]
    _material: Arc<BudgetViewGuard>,
}
impl PreparedTaskStopResultCommand {
    pub(super) fn scope(&self) -> &InputEvidenceScope {
        self.address.scope()
    }
    pub(super) fn message(&self) -> &MessageEnvelope {
        &self.plan.message
    }
    pub(super) fn restrict_runtime(&mut self) {
        self.ready_runtime_only = true;
    }
}
impl PreparedInputTransition {
    /// Local fault injection is excluded from the frozen command identity.
    #[cfg(test)]
    pub(crate) fn with_task_stop_result_fault(
        mut self,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        anyhow::ensure!(
            fault.is_none()
                || matches!(
                    fault,
                    Some(
                        TransitionFaultPoint::BeforeCommit
                            | TransitionFaultPoint::BeforeCacheUpdate
                    )
                ),
            "unsupported TaskStop Result local fault point"
        );
        let InputTransitionCommand::TaskStopContinuation(command) = self.command.as_mut() else {
            anyhow::bail!("Result fault requires its original Stop continuation");
        };
        command.fault = fault;
        Ok(self)
    }
    pub(crate) fn task_stop_result(
        expected: &stops::TaskStopRecord,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let plan = expected
            .intent
            .result
            .as_ref()
            .context("Stop has no original Result template")?;
        // No current progress/revision or new ID/time enters this original intent.
        let bytes = usize::try_from(serialized_bytes(&(
            &expected.prepared,
            &expected.intent.task.scope,
            expected.effect_seq,
            &expected.intent_digest,
            &expected.generation_digest,
            &expected.ack_task_digest,
            plan,
        ))?)?;
        let material = super::task_stop_phase::charge(
            ledger,
            &expected.intent.task.scope,
            request,
            bytes,
            8,
            8,
        )?;
        let command = PreparedTaskStopResultCommand {
            address: stops::TaskStopResultAddress::from_record(expected)?,
            plan: plan.clone(),
            ready_runtime_only: false,
            fault: None,
            _material: material,
        };
        Self::prepare(
            expected.intent.task.scope.clone(),
            plan.phase_nonce.clone(),
            plan.message.created_at,
            InputTransitionCommand::TaskStopContinuation(command),
        )
    }
}

pub(super) fn commit_result_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    command: &PreparedTaskStopResultCommand,
    work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let scope = command.scope();
    anyhow::ensure!(
        prepared.primary_scope == *scope
            && prepared.descriptor.scopes() == std::slice::from_ref(scope)
            && prepared.ended_memory.is_none()
            && prepared.original_dispatch_claim.is_none(),
        "Stop Result can only write its original parent group"
    );
    anyhow::ensure!(
        prepared.memory.as_ref().is_none_or(|memory| matches!(
            memory,
            crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Deferred { .. }
        )),
        "Stop Result cannot start/finish a dispatch"
    );
    let receipt_guard = super::super::prepared_task_result::receipt_budget_tx(
        tx,
        &prepared.descriptor,
        scope,
        request,
        ledger,
    )?;
    if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)? {
        let InputMutationResult::TaskStopPhase { ack } = &receipt.outcome.result else {
            anyhow::bail!("Stop Result receipt has another family")
        };
        anyhow::ensure!(
            ack.result.is_some(),
            "Stop Result receipt has another phase"
        );
        let mut guards = vec![receipt_guard];
        if command.ready_runtime_only {
            guards.push(super::task_stop_phase::charge(
                ledger,
                scope,
                request,
                stops::task_stop_phase_identity_material_tx(tx, scope)?,
                1,
                8,
            )?);
            anyhow::ensure!(
                ack.install_mode == stops::TaskStopPhaseInstallMode::Current
                    && stops::observe_task_stop_phase_mode_tx(tx, scope)?
                        == stops::TaskStopPhaseInstallMode::Current,
                stops::TaskStopPhaseError::RequiresMetadataActor
            );
        }
        return Ok(InputMutationExecution::Replayed {
            receipt,
            _material_budget_views: guards,
        });
    }
    let mut guards = vec![
        receipt_guard,
        super::task_stop_phase::charge(
            ledger,
            scope,
            request,
            stops::task_stop_phase_identity_material_tx(tx, scope)?,
            1,
            8,
        )?,
    ];
    let mode = stops::observe_task_stop_phase_mode_tx(tx, scope)?;
    anyhow::ensure!(
        !command.ready_runtime_only || mode == stops::TaskStopPhaseInstallMode::Current,
        stops::TaskStopPhaseError::RequiresMetadataActor
    );
    let mut memory_budget = prepared
        .memory
        .as_ref()
        .map(|memory| {
            crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget::prepare_tx(
                tx,
                memory,
                work_request,
                ledger,
            )
        })
        .transpose()?;
    let selected = stops::prepare_task_stop_result_tx(
        tx,
        &command.address,
        &command.plan,
        |bytes, records| {
            if let Some(budget) = &mut memory_budget {
                budget.additional_material(bytes, records, 8)?;
            }
            super::task_stop_phase::charge(ledger, scope, request, bytes, records, 8)
        },
    )?;
    guards.push(selected.budget_view());
    // Own original Task predicate material is counted before SQLite JSON checks;
    // no foreign Task body will be decoded to decide this old generation's fate.
    if matches!(
        mode,
        stops::TaskStopPhaseInstallMode::Current | stops::TaskStopPhaseInstallMode::Incomplete
    ) {
        let task_bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(owner_agent_id AS BLOB)) FROM tasks WHERE task_id=?1),0)",
        [&selected.record().ack_task.id],|r|r.get(0))?;
        guards.push(super::task_stop_phase::charge(
            ledger, scope, request, task_bytes, 1, 8,
        )?);
    }
    let unavailable = match mode {
        stops::TaskStopPhaseInstallMode::NoState => {
            Some(stops::TaskStopResultUnavailable::OwnerNoState)
        }
        stops::TaskStopPhaseInstallMode::Retired => {
            Some(stops::TaskStopResultUnavailable::ScopeRetired)
        }
        _ if !stops::original_task_generation_matches_tx(tx, selected.record())? => {
            Some(stops::TaskStopResultUnavailable::TaskGenerationChanged)
        }
        _ => None,
    };
    // Retired never reads State; sparse delivery still requires real State and
    // preserves TaskNone instead of hydrating an unrelated complete domain.
    let (mut value, actual, sparse_invalidation) = if let Some(reason) = unavailable {
        let proof = if mode == stops::TaskStopPhaseInstallMode::Current {
            let material = tasks::task_maintenance_material_tx(tx, scope, &BTreeSet::new())?;
            guards.push(super::task_stop_phase::charge(
                ledger,
                scope,
                request,
                material.bytes,
                material.records,
                1,
            )?);
            let bytes: usize = tx.query_row(
                "SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1",
                [&scope.agent_id],
                |r| r.get(0),
            )?;
            guards.push(super::task_stop_phase::charge(
                ledger, scope, request, bytes, 1, 8,
            )?);
            let proof = tasks::prepare_original_task_cleanup_tx(tx, scope, &BTreeSet::new())?;
            tasks::preflight_task_write_snapshot_with_memory_budget_tx(
                tx,
                &proof,
                request,
                ledger,
                memory_budget.as_mut(),
            )?
            .require_current()?;
            Some(proof)
        } else {
            None
        };
        let actual = stops::acknowledge_task_stop_result_tx(
            selected,
            stops::TaskStopResultOutcome::Unavailable { reason },
        )?;
        let mut value = TransitionCommit::default();
        if let Some(proof) = proof {
            let state = agent_state_tx(tx, &scope.agent_id)?
                .context("Stop unavailable current State missing")?;
            let facts = tasks::register_task_writes_with_stop_result_observation_tx(
                tx,
                &proof,
                &BTreeSet::new(),
                &state,
                request,
                ledger,
                memory_budget.as_mut(),
                &actual,
            )?;
            guards.push(ledger.register_task_view(
                scope,
                InputMemoryCategory::Candidate,
                facts.delta.records.iter(),
            )?);
            guards.push(ledger.register_task_stop_view(
                scope,
                InputMemoryCategory::Candidate,
                facts.delta.stop_records.iter(),
            )?);
            guards.push(ledger.register_owned_bytes(
                scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: serialized_bytes(&facts.delta.coverage)?,
                },
            )?);
            value.effects.task_delta = Some(facts.delta);
            value.effects.task_delta_requires_prepared_domain = true;
        }
        (value, actual, None)
    } else {
        let frozen_task = &selected.record().ack_task;
        let pending = result::TaskResultSettlementRecord::pending(
            frozen_task,
            &command.plan.message,
            command.plan.message.created_at,
        )?;
        guards.extend(super::task_writes::prepare_stop_result_material_tx(
            tx,
            scope,
            frozen_task,
            pending.as_ref(),
            &command.plan.message,
            request,
            ledger,
            memory_budget.as_mut(),
        )?);
        let original_task_proof = tasks::prepare_original_task_cleanup_tx(
            tx,
            scope,
            &BTreeSet::from([TaskKey::new(TaskFamily::Task, &frozen_task.id)]),
        )?;
        let pending_command = if let Some(pending) = &pending {
            Some(result::prepare_upsert_pending_tx(
                tx,
                frozen_task,
                pending,
                result::TaskResultWriteLimits {
                    record_limit: request.record_limit,
                    byte_limit: request.byte_limit,
                },
            )?)
        } else {
            None
        };
        let cleanup = if let (Some(pending), Some(command)) = (&pending, &pending_command) {
            let keys = BTreeSet::from([
                TaskKey::new(TaskFamily::Task, &frozen_task.id),
                TaskKey::new(TaskFamily::ResultSettlement, &pending.result_identity),
            ]);
            let maintenance = tasks::prepare_existing_task_maintenance_tx(tx, scope, &keys)?;
            Some(tasks::prepare_original_task_result_cleanup_tx(
                tx,
                maintenance,
                tasks::OriginalTaskResultCleanupSource::Stop(
                    selected.original_cleanup_view(&prepared.descriptor)?,
                ),
                command,
            )?)
        } else {
            None
        };
        let input =
            crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(
                tx,
                scope,
                BTreeSet::new(),
                BTreeSet::new(),
            )?;
        let input = if let Some(cleanup) = &cleanup {
            crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_from_task_result_cleanup_tx(tx,input,cleanup)?
        } else {
            crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_from_task_stop_message_tx(
                tx,input,&selected.original_cleanup_view(&prepared.descriptor)?,&original_task_proof)?
        };
        let queue = QueueEntryRecord {
            message_id: command.plan.message.id.clone(),
            agent_id: scope.agent_id.clone(),
            priority: command.plan.message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: command.plan.message.created_at,
            updated_at: command.plan.message.created_at,
        };
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT agent_id,status FROM queue_entries WHERE message_id=?1",
                [&queue.message_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        anyhow::ensure!(
            previous
                .as_ref()
                .is_none_or(|(owner, status)| owner == &scope.agent_id
                    && matches!(status.as_str(), "queued" | "interrupted")),
            "Stop Result cannot requeue a foreign, claimed or terminal input"
        );
        let (predicted, projection_guard) = predicted_trigger_wait_tx(
            tx,
            scope,
            &command.plan.message,
            &queue,
            work_request,
            ledger,
            memory_budget.as_mut(),
        )?;
        guards.extend(projection_guard);
        let waits: Vec<_> = predicted.into_iter().collect();
        let mut work_keys = crate::runtime_db::work_snapshot::work_wait_write_keys_tx(tx, &waits)?;
        if let Some(work) = &command.plan.message.work_item_id {
            work_keys.insert(WorkKey::new(WorkFamily::Work, work));
        }
        if mode == stops::TaskStopPhaseInstallMode::Current {
            tasks::preflight_task_write_snapshot_with_memory_budget_tx(
                tx,
                &original_task_proof,
                request,
                ledger,
                memory_budget.as_mut(),
            )?
            .require_current()?;
        }
        let work_proof = crate::runtime_db::work_snapshot::prepare_original_work_write_tx(
            tx, scope, &work_keys,
        )?;
        let preflight_request = memory_budget
            .as_ref()
            .map(|budget| budget.work_request(work_request))
            .transpose()?
            .unwrap_or_else(|| work_request.clone());
        crate::runtime_db::work_snapshot::preflight_work_write_snapshot_tx(
            tx,
            &work_proof,
            &preflight_request,
        )?
        .require_current()?;
        let detached = match &prepared.memory {
            Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Deferred {
                binding,
                frozen,
            }) => {
                let token = crate::runtime_db::dispatch_memory::prepare_detached_memory_tx(
                    tx,
                    &work_proof,
                    &prepared.descriptor,
                    frozen,
                )?
                .context("Stop Result Deferred omitted actual StartedTurn")?;
                crate::runtime_db::dispatch_memory::validate_detached_binding(&token, binding)?;
                Some(token)
            }
            _ => None,
        };
        let state_bytes: usize = tx.query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1",
            [&scope.agent_id],
            |r| r.get(0),
        )?;
        guards.push(super::task_stop_phase::charge(
            ledger,
            scope,
            request,
            state_bytes,
            1,
            8,
        )?);
        let old = agent_state_tx(tx, &scope.agent_id)?
            .context("Stop Result actual owner State missing")?;
        let count:usize=tx.query_row("SELECT COUNT(*) FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted')",[&scope.agent_id],|r|r.get(0))?;
        anyhow::ensure!(
            old.pending == count,
            "Stop Result original pending accounting differs"
        );
        let mut next = old.clone();
        next.pending = count
            .checked_add(usize::from(previous.is_none()))
            .context("Stop Result pending overflow")?;
        let state = AgentStateMutation {
            expected: Some(Box::new(old)),
            record: Box::new(next),
        };
        validate_agent_state_mutation_tx(tx, Some(&state))?;
        let triggered = trigger_wait_and_enqueue_at_tx(
            tx,
            &command.plan.message,
            &queue,
            command.plan.message.created_at,
        )?;
        let (actual_result_ids, pending_changed) = if let Some(pending_command) = pending_command {
            let pending_ack = pending_command.execute_original_task_cleanup(
                cleanup.as_ref().context("Stop pending cleanup missing")?,
            )?;
            let result::TaskResultWriteOutcome::Changed(changed) = pending_ack.outcome else {
                anyhow::bail!("Stop pending Result returned another operation")
            };
            (pending_ack.actual_result_ids, changed)
        } else {
            (BTreeSet::new(), false)
        };
        apply_agent_state_mutation_tx(tx, Some(&state))?;
        let mut index = Vec::new();
        if triggered.message_inserted {
            index.push(RuntimeIndexChange::for_message(&triggered.message));
        }
        let mut effects = PostCommitEffects {
            agent_state: Some(state),
            messages: vec![triggered.message.clone()],
            queued_messages: vec![triggered.message.clone()],
            wait_condition_writes: triggered.wait_writes.clone(),
            notify_scheduler: true,
            ..Default::default()
        };
        effects.task_write_keys.extend(
            actual_result_ids
                .iter()
                .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
        );
        let mut value = finish_transition_with_cleanup_tx(
            tx,
            Some(scope),
            Some(&input),
            true,
            &scope.agent_id,
            &[],
            &index,
            None,
            effects,
        )?;
        value.applied |= pending_changed || triggered.applied();
        value.effects.audit_events.extend(triggered.audit_events);
        let actual_work: BTreeSet<_> = value
            .effects
            .wait_condition_writes
            .wait_ids
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Wait, id))
            .collect();
        if let Some(budget) = &mut memory_budget {
            budget.before_finalizer_tx(tx)?;
        }
        let final_request = memory_budget
            .as_ref()
            .map(|budget| budget.work_request(work_request))
            .transpose()?
            .unwrap_or_else(|| work_request.clone());
        let final_state =
            agent_state_tx(tx, &scope.agent_id)?.context("Stop Result final State missing")?;
        let work = crate::runtime_db::work_snapshot::register_work_writes_tx(
            tx,
            &work_proof,
            &actual_work,
            &BTreeSet::new(),
            &final_state,
            &final_request,
        )?;
        value.effects.work_input_preparation = Some(WorkInputPreparation {
            scope: scope.clone(),
            work_revision: work.delta.revision,
            owning_roots: work.owning_input_roots,
            completion_heads: work
                .delta
                .coverage
                .latest_results
                .iter()
                .filter_map(|(id, head)| head.clone().map(|head| (id.clone(), head)))
                .collect(),
        });
        value.effects.work_delta = Some(work.delta);
        if let Some(token) = detached {
            if !actual_work.is_empty() {
                let mut refs = prepared.memory.as_ref().expect("Deferred").input_roots();
                refs.insert(
                    crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                        crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                        &command.plan.message.id,
                    ),
                );
                if let Some(work) = &value.effects.work_input_preparation {
                    refs.extend(work.owning_roots.iter().cloned());
                }
                if let Some(budget) = &mut memory_budget {
                    budget.detached_member(&actual_work, &refs, &prepared.descriptor)?;
                }
                crate::runtime_db::dispatch_memory::record_detached_memory_tx(
                    token,
                    actual_work.clone(),
                    refs,
                )?;
            }
        }
        let message_seq = triggered
            .message
            .message_seq
            .context("Stop Result actual Message omitted assigned sequence")?;
        let (task_proof, sparse) = if let Some(cleanup) = cleanup {
            if mode == stops::TaskStopPhaseInstallMode::Current {
                (
                    Some(cleanup.into_task_write_proof(tx, &actual_result_ids)?),
                    None,
                )
            } else {
                (
                    None,
                    Some(tasks::register_original_task_result_cleanup_tx(
                        tx,
                        cleanup,
                        &value.effects.task_write_keys,
                    )?),
                )
            }
        } else if mode == stops::TaskStopPhaseInstallMode::Current {
            (Some(original_task_proof), None)
        } else {
            (None, None)
        };
        let actual = stops::acknowledge_task_stop_result_tx(
            selected,
            stops::TaskStopResultOutcome::Delivered {
                message_id: command.plan.message.id.clone(),
                message_seq,
                result_identity: pending.map(|pending| pending.result_identity),
            },
        )?;
        if let Some(proof) = task_proof {
            let facts = tasks::register_task_writes_with_stop_result_observation_tx(
                tx,
                &proof,
                &value.effects.task_write_keys,
                &final_state,
                request,
                ledger,
                memory_budget.as_mut(),
                &actual,
            )?;
            guards.push(ledger.register_task_view(
                scope,
                InputMemoryCategory::Candidate,
                facts.delta.records.iter(),
            )?);
            guards.push(ledger.register_task_stop_view(
                scope,
                InputMemoryCategory::Candidate,
                facts.delta.stop_records.iter(),
            )?);
            guards.push(ledger.register_owned_bytes(
                scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: serialized_bytes(&facts.delta.coverage)?,
                },
            )?);
            value.effects.task_delta = Some(facts.delta);
            value.effects.task_delta_requires_prepared_domain = true;
        }
        let invalidation = sparse.and_then(|ack| ack.invalidation).map(|change| {
            stops::TaskStopDomainInvalidation {
                base_revision: change.base_revision,
                revision: change.revision,
            }
        });
        (value, actual, invalidation)
    };
    let invalidation = if value.effects.task_delta.is_some() {
        None
    } else if sparse_invalidation.is_some() {
        sparse_invalidation
    } else {
        tasks::invalidate_task_scope_for_stop_progress_tx(tx, actual.actual())?
    };
    guards.push(actual.actual().budget_view());
    value.applied |= actual.actual().changed();
    value.effects.task_material_budget_views.extend(guards);
    let mut ack = actual.actual().ack(invalidation, mode);
    ack.result = Some(actual.result_ack().clone());
    let mut install_keys =
        crate::runtime_db::input_recovery_install::InputMutationInstallKeys::from_effects(
            scope,
            &value.effects,
        )?;
    if let Some(work) = &value.effects.work_input_preparation {
        install_keys
            .input_roots
            .extend(work.owning_roots.iter().cloned());
        install_keys.completion_brief_ids.extend(
            work.completion_heads
                .values()
                .map(|head| head.brief_id.clone()),
        );
    }
    install_keys.validate()?;
    let outcome = InputMutationOutcome {
        result: InputMutationResult::TaskStopPhase { ack: Box::new(ack) },
        applied: value.applied,
        delivery_receipt: None,
        scopes: vec![InputMutationScopeOutcome {
            scope: scope.clone(),
            install_keys,
            audit_events: value.effects.audit_events.clone(),
            notify_memory_index: value.effects.notify_memory_index,
            notify_scheduler: value.effects.notify_scheduler,
        }],
    };
    if let Some(base) = &prepared.auto_base_descriptor {
        crate::runtime_db::dispatch_memory::insert_original_auto_memory_tx(
            tx,
            base,
            &prepared.descriptor,
            prepared
                .memory
                .as_ref()
                .context("automatic Stop Result omitted intent")?,
        )?;
    }
    value.effects.fault = command.fault;
    inject_fault(command.fault, TransitionFaultPoint::BeforeCommit)?;
    let receipt = insert_input_mutation_receipt_tx(
        tx,
        &prepared.descriptor,
        &outcome,
        command.plan.message.created_at,
    )?;
    Ok(InputMutationExecution::Committed { value, receipt })
}
