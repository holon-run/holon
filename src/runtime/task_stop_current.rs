//! Original local Stop leases survive caller loss and later metadata phases.
use super::*;
use super::{
    input_evidence_budget::{BudgetViewGuard, InputMemoryCategory},
    task_stop_effects::PublishedTaskStopEffects,
};
use crate::runtime_db::{
    task_snapshot::TaskDomainLoadRequest,
    task_stop::{
        load_task_stop_by_sequence_tx, TaskStopChildState, TaskStopPhysicalState,
        TaskStopProgressAction, TaskStopRecord, TaskStopResultState, TaskStopWorktreeState,
    },
    transitions::PreparedInputTransition,
};

enum LocalStopPhase {
    Result,
    Monitor(Arc<task_stop_join::TaskStopMonitorJoin>),
    Safe(TaskStopProgressAction),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::StubProvider;

    #[tokio::test]
    async fn original_owned_stop_completes_real_monitor_and_lost_initial_ack_without_a_second_result(
    ) -> Result<()> {
        for (fault, no_handle) in [
            (None, false),
            (Some(TransitionFaultPoint::BeforeCommit), false),
            (Some(TransitionFaultPoint::BeforeCacheUpdate), false),
            (Some(TransitionFaultPoint::BeforeCacheUpdate), true),
        ] {
            let home = tempfile::tempdir()?;
            let runtime = RuntimeHandle::new(
                "default",
                home.path().into(),
                None::<crate::types::WorkspaceEntry>,
                String::new(),
                Arc::new(StubProvider::new("unused")),
                "default".into(),
                crate::context::ContextConfig::default(),
            )?;
            let at = runtime.now();
            let task = TaskRecord {
                id: "task:original-owned-complete".into(),
                agent_id: "default".into(),
                kind: if no_handle {
                    TaskKind::CommandTask
                } else {
                    TaskKind::ActorInvocation
                },
                status: TaskStatus::Running,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: None,
                detail: Some(serde_json::json!({"cancel_requested":false})),
                recovery: None,
            };
            runtime
                .persist_task_transition(&task, "original_owned_complete_seed")
                .await?;
            if !no_handle {
                let lease = runtime.inner.control_cancellations.task(&task);
                lease.attach(tokio::spawn(std::future::pending::<()>()));
                runtime.inner.task_handles.lock().await.insert(
                    task.id.clone(),
                    command_task::ManagedTaskHandle::Async(
                        control_cancellation::SharedControlTaskHandle(lease),
                    ),
                );
            }
            if let Some(fault) = fault {
                runtime.inject_next_transition_fault(fault);
            }
            let answer = runtime
                .stop_task(&task.id, &AuthorityClass::OperatorInstruction)
                .await;
            assert_eq!(answer.is_err(), fault.is_some());
            if fault == Some(TransitionFaultPoint::BeforeCommit) {
                let (stops, messages): (u64, u64) = runtime.runtime_db().connection()?.query_row(
                    "SELECT (SELECT COUNT(*) FROM task_stop_effects),(SELECT COUNT(*) FROM messages WHERE kind='task_result')",
                    [], |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!((stops, messages), (0, 0));
                let handles = runtime.inner.task_handles.lock().await;
                assert!(
                    !handles[&task.id].control_lease().unwrap().is_finished(),
                    "proven rollback cannot abort the original monitor"
                );
                drop(handles);
                let retry = runtime
                    .stop_task(&task.id, &AuthorityClass::OperatorInstruction)
                    .await?;
                assert_eq!(retry.status, TaskStatus::Cancelled);
            }
            runtime.join_owned_mutations().await?;
            // This is the already-owned Host retry path, after original
            // admission closes. It must not recreate a Stop descriptor.
            runtime.continue_retained_task_stop_plans_owned().await?;
            runtime.continue_retained_task_stop_plans_owned().await?;
            let (stops,messages,physical,result):(u64,u64,String,String)=runtime.runtime_db().connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM task_stop_effects),(SELECT COUNT(*) FROM messages WHERE kind='task_result'),physical_state,result_state FROM task_stop_effects WHERE task_id=?1",
                [&task.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            assert_eq!(stops, 1);
            assert_eq!(messages, if no_handle { 0 } else { 1 });
            assert!(
                !runtime
                    .inner
                    .task_handles
                    .lock()
                    .await
                    .contains_key(&task.id),
                "actual original monitor join must release only its exact handle"
            );
            assert_eq!(
                physical,
                if no_handle {
                    "missing_proof"
                } else {
                    "monitor_joined"
                }
            );
            assert_eq!(
                result,
                if no_handle {
                    "not_required"
                } else {
                    "completed"
                }
            );
            let scope = resident_input_evidence::InputEvidenceScope {
                agent_id: "default".into(),
                identity_incarnation: 1,
            };
            let (plans, _guard) = runtime
                .inner
                .task_stop_effects
                .retained_plans(&runtime.inner.input_evidence_budget, &scope)?;
            assert!(
                plans.is_empty(),
                "positive terminal whole ACK must release only its original local plan"
            );
        }
        Ok(())
    }
}
impl LocalStopPhase {
    fn required(&self, record: &TaskStopRecord) -> bool {
        match self {
            Self::Result => record.progress.result == TaskStopResultState::Pending,
            Self::Monitor(_) | Self::Safe(TaskStopProgressAction::PhysicalAttention { .. }) => {
                record.progress.physical == TaskStopPhysicalState::Pending
            }
            Self::Safe(TaskStopProgressAction::ChildAttention { .. }) => {
                record.progress.child == TaskStopChildState::Pending
            }
            Self::Safe(TaskStopProgressAction::WorktreeRetained { .. }) => {
                record.progress.worktree == TaskStopWorktreeState::Pending
            }
            Self::Safe(_) => false,
        }
    }
    fn prepare(
        self,
        record: &TaskStopRecord,
        request: &TaskDomainLoadRequest,
        ledger: &input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Option<PreparedInputTransition>> {
        if !self.required(record) {
            return Ok(None);
        }
        Ok(Some(match self {
            Self::Result => PreparedInputTransition::task_stop_result(record, request, ledger)?,
            Self::Monitor(joined) => PreparedInputTransition::task_stop_monitor_joined(
                record,
                crate::ids::audit_event_id(),
                Utc::now(),
                joined,
                request,
                ledger,
            )?,
            Self::Safe(action) => PreparedInputTransition::task_stop_safe_progress(
                record,
                crate::ids::audit_event_id(),
                Utc::now(),
                action,
                request,
                ledger,
            )?,
        }))
    }
}

fn same_original(expected: &TaskStopRecord, actual: &TaskStopRecord) -> Result<()> {
    anyhow::ensure!(
        expected.prepared == actual.prepared
            && expected.intent == actual.intent
            && expected.receipt_binding() == actual.receipt_binding(),
        "local TaskStop immutable address differs"
    );
    Ok(())
}

impl RuntimeHandle {
    /// Named preparation is limited to the already registered current Runtime.
    pub(crate) async fn prepare_current_task_stop_publication(
        &self,
        original: &TaskStopRecord,
        requested: &TaskDomainLoadRequest,
    ) -> Result<()> {
        let mut mutation = self.begin_mutation().await?;
        anyhow::ensure!(
            mutation.input_evidence_scope() == original.intent.task.scope,
            "current Stop preparation namespace differs"
        );
        let (mut load, _existing_selectors) = mutation.owner_task_load_request()?;
        let metadata = input_evidence_budget::serialized_bytes(&requested.capture.required_keys)?
            .checked_mul(4)
            .context("Stop publication selectors overflow")?;
        anyhow::ensure!(
            metadata <= u64::try_from(load.byte_limit)?
                && load
                    .capture
                    .required_keys
                    .len()
                    .checked_add(requested.capture.required_keys.len())
                    .and_then(|n| n.checked_mul(4))
                    .is_some_and(|n| n <= load.record_limit),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "Stop publication selectors"
            )
        );
        let _selectors = self.inner.input_evidence_budget.register_owned_bytes(
            &original.intent.task.scope,
            InputMemoryCategory::Candidate,
            input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: metadata,
            },
        )?;
        load.capture
            .required_keys
            .extend(requested.capture.required_keys.iter().cloned());
        load.capture.terminal_limit = load
            .capture
            .terminal_limit
            .max(requested.capture.terminal_limit);
        mutation.prepare_owner_task_request(&load.capture).await?;
        Ok(())
    }

    /// An inventory lookup only; never a lookup of a successor Task handle.
    pub(crate) fn has_original_task_stop_plan(&self, original: &TaskStopRecord) -> Result<bool> {
        Ok(self
            .inner
            .task_stop_effects
            .find_original(original)?
            .is_some())
    }

    async fn commit_local_task_stop_phase(
        &self,
        original: &TaskStopRecord,
        phase: LocalStopPhase,
        request: &TaskDomainLoadRequest,
    ) -> Result<()> {
        let ledger = &self.inner.input_evidence_budget;
        if let Some(bridge) = &self.inner.host_bridge {
            let current = bridge
                .read_original_task_stop_metadata(original, request)
                .await?;
            let Some(prepared) = phase.prepare(current.record(), request, ledger)? else {
                return Ok(());
            };
            let result = bridge
                .commit_task_stop_phase_metadata(prepared, request.clone())
                .await?;
            anyhow::ensure!(
                result.ack.original_nonce == original.prepared.command_nonce()
                    && result.ack.binding == original.receipt_binding(),
                "local TaskStop phase ACK differs from original"
            );
            return Ok(());
        }
        let mut mutation = self.begin_mutation().await?;
        anyhow::ensure!(
            mutation.input_evidence_scope() == original.intent.task.scope,
            "standalone TaskStop original namespace differs"
        );
        let current = self.inner.runtime_db.transaction(|tx| {
            load_task_stop_by_sequence_tx(
                tx,
                &original.intent.task.scope,
                original.effect_seq,
                request.byte_limit,
                ledger,
            )
        })?;
        let actual = current
            .records
            .first()
            .context("local TaskStop original disappeared")?;
        same_original(original, actual)?;
        let Some(prepared) = phase.prepare(actual, request, ledger)? else {
            return Ok(());
        };
        let result = mutation.commit_current_task_stop_phase(prepared).await?;
        anyhow::ensure!(
            result.ack.original_nonce == original.prepared.command_nonce()
                && result.ack.binding == original.receipt_binding(),
            "standalone TaskStop phase ACK differs from original"
        );
        Ok(())
    }

    async fn release_observed_task_stop_if_complete(
        &self,
        original: &TaskStopRecord,
        request: &TaskDomainLoadRequest,
    ) -> Result<()> {
        let Some(plan) = self.inner.task_stop_effects.find_original(original)? else {
            return Ok(());
        };
        let complete = if let Some(bridge) = &self.inner.host_bridge {
            let current = bridge
                .read_original_task_stop_metadata(original, request)
                .await?;
            !current.record().progress.is_pending()
        } else {
            let _mutation = self.begin_mutation().await?;
            let current = self.inner.runtime_db.transaction(|tx| {
                load_task_stop_by_sequence_tx(
                    tx,
                    &original.intent.task.scope,
                    original.effect_seq,
                    request.byte_limit,
                    &self.inner.input_evidence_budget,
                )
            })?;
            let actual = current
                .records
                .first()
                .context("completed original TaskStop disappeared")?;
            same_original(original, actual)?;
            !actual.progress.is_pending()
        };
        if complete {
            plan.finished
                .store(true, std::sync::atomic::Ordering::Release);
            self.inner.task_stop_effects.release_finished(&plan)?;
        }
        Ok(())
    }

    /// The initial owned worker already registered before its first await.
    pub(super) async fn continue_published_task_stop_owned(
        &self,
        original: Arc<TaskStopRecord>,
        published: PublishedTaskStopEffects,
        worker: task_stop_effects::OriginalTaskStopWorker,
    ) -> Result<()> {
        let mutation = self.begin_mutation().await?;
        let (request, _selectors) = mutation.owner_task_load_request()?;
        drop(mutation);
        Box::pin(self.continue_published_task_stop_inner(
            original,
            published,
            request,
            Some(worker),
        ))
        .await
    }

    async fn continue_published_task_stop_inner(
        &self,
        original: Arc<TaskStopRecord>,
        published: PublishedTaskStopEffects,
        request: TaskDomainLoadRequest,
        worker: Option<task_stop_effects::OriginalTaskStopWorker>,
    ) -> Result<()> {
        let plan = self
            .inner
            .task_stop_effects
            .find_original(&original)?
            .context("original TaskStop local responsibility disappeared")?;
        let _worker = match worker {
            Some(worker) => worker,
            None => match plan.claim_worker() {
                Some(worker) => worker,
                None => return Ok(()),
            },
        };
        // A reused positive initial ACK remains bound to this exact old lease.
        // The per-plan one-shot prevents another worker from upgrading it.
        published.signal_original();
        let result_error = self
            .commit_local_task_stop_phase(&original, LocalStopPhase::Result, &request)
            .await
            .err();
        // Joining holds no mutation/SQLite lock and proves only the old monitor.
        let physical = match published
            .join_original(&self.inner.input_evidence_budget)
            .await
        {
            Ok(Some(joined)) => {
                self.release_completed_task_handle_exact(
                    &original.intent.task.key.id,
                    original
                        .intent
                        .lease_id
                        .as_deref()
                        .context("joined original Stop omitted its frozen lease")?,
                )
                .await;
                LocalStopPhase::Monitor(Arc::new(joined))
            }
            Ok(None) => LocalStopPhase::Safe(TaskStopProgressAction::PhysicalAttention {
                reason: "original monitor lease unavailable".into(),
            }),
            Err(error) => {
                tracing::warn!(effect_seq=original.effect_seq,error=%error,"original TaskStop monitor join failed");
                LocalStopPhase::Safe(TaskStopProgressAction::PhysicalAttention {
                    reason: "original monitor join failed".into(),
                })
            }
        };
        // On an unknown phase outcome, retain the exact plan for recovery. A
        // cold fallback must not overwrite a still-owned physical obligation.
        self.commit_local_task_stop_phase(&original, physical, &request)
            .await?;
        let continuation = if let Some(bridge) = &self.inner.host_bridge {
            let source = self.inner.input_evidence_budget.register_task_stop_view(
                &original.intent.task.scope,
                InputMemoryCategory::Pins,
                std::iter::once(&original),
            )?;
            super::continue_original_task_stop_metadata(
                bridge,
                original.clone(),
                source,
                request.clone(),
            )
            .await
        } else {
            self.commit_local_task_stop_phase(
                &original,
                LocalStopPhase::Safe(TaskStopProgressAction::ChildAttention {
                    reason: "original Host unavailable".into(),
                }),
                &request,
            )
            .await?;
            self.commit_local_task_stop_phase(
                &original,
                LocalStopPhase::Safe(TaskStopProgressAction::WorktreeRetained {
                    reason: "original Host producer proof unavailable".into(),
                }),
                &request,
            )
            .await
        };
        self.release_observed_task_stop_if_complete(&original, &request)
            .await?;
        continuation?;
        if let Some(error) = result_error {
            return Err(error);
        }
        Ok(())
    }

    /// Called by an already-owned Host worker, including during Closing.
    pub(crate) async fn continue_original_task_stop_loaded_owned(
        &self,
        original: Arc<TaskStopRecord>,
        source: Arc<BudgetViewGuard>,
        request: TaskDomainLoadRequest,
    ) -> Result<()> {
        let _source = source;
        let Some(_plan) = self.inner.task_stop_effects.find_original(&original)? else {
            let bridge = self
                .inner
                .host_bridge
                .as_ref()
                .context("original Stop Host unavailable")?;
            return super::continue_original_task_stop_metadata(bridge, original, _source, request)
                .await;
        };
        if !original.progress.is_pending() {
            return self
                .release_observed_task_stop_if_complete(&original, &request)
                .await;
        }
        let published = match self
            .inner
            .task_stop_effects
            .resume_published(&original, &self.inner.input_evidence_budget)?
        {
            Some(published) => published,
            None => {
                if let Some(bridge) = &self.inner.host_bridge {
                    let current = bridge
                        .read_original_task_stop_metadata(&original, &request)
                        .await?;
                    if current.owner_state_unavailable() {
                        let result = super::continue_original_task_stop_metadata(
                            bridge,
                            current.shared_record(),
                            _source,
                            request.clone(),
                        )
                        .await;
                        self.release_observed_task_stop_if_complete(&original, &request)
                            .await?;
                        return result;
                    }
                }
                if let Some(bridge) = &self.inner.host_bridge {
                    // The callback holds the real current owner while it
                    // compares publication to the fresh original whole ACK.
                    // It cannot install state or pick a new local Task handle.
                    match bridge
                        .with_original_task_stop_metadata(
                            &original,
                            &request,
                            |reservation, current| {
                                self.inner
                                    .task_stop_effects
                                    .confirm_original_receipt_publication(
                                        reservation,
                                        current,
                                        &self.inner.input_evidence_budget,
                                    )
                            },
                        )
                        .await
                    {
                        Ok(published) => published,
                        Err(error)
                            if error
                                .downcast_ref::<resident_task::TaskReadError>()
                                .is_some() =>
                        {
                            bridge
                                .prepare_original_task_stop_publication(&original, &request)
                                .await?;
                            bridge
                                .with_original_task_stop_metadata(
                                    &original,
                                    &request,
                                    |reservation, current| {
                                        self.inner
                                            .task_stop_effects
                                            .confirm_original_receipt_publication(
                                                reservation,
                                                current,
                                                &self.inner.input_evidence_budget,
                                            )
                                    },
                                )
                                .await?
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    // A lost initial publication is reconciled and prepared under
                    // its original owner before any physical capability is minted.
                    let mut mutation = self.begin_mutation().await?;
                    mutation
                        .prepare_owner_task_request(&request.capture)
                        .await?;
                    self.inner.task_stop_effects.confirm_published(
                        &self.inner.mutation_owner,
                        &original,
                        &self.inner.input_evidence_budget,
                    )?
                }
            }
        };
        Box::pin(self.continue_published_task_stop_inner(original, published, request, None)).await
    }

    /// A finite Host sweep also visits retained initial attempts whose complete
    /// Stop is absent from the pending inventory. The original worker cannot be
    /// forgotten merely because its caller lost the initial ACK.
    pub(crate) async fn continue_retained_task_stop_plans_owned(&self) -> Result<()> {
        let scope = resident_input_evidence::InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let ledger = &self.inner.input_evidence_budget;
        let (plans, _selectors) = self
            .inner
            .task_stop_effects
            .retained_plans(ledger, &scope)?;
        let mut failed = false;
        for plan in plans {
            if !plan.sql_finished() {
                continue;
            }
            let result = async {
                anyhow::ensure!(plan.intent().task.scope == scope,
                    "retained TaskStop belongs to another original instance");
                if let Some(bridge) = &self.inner.host_bridge {
                    // An old or unavailable Runtime must not claim/hydrate a
                    // current owner just to recover its original obligation.
                    let limits=ledger.limits();
                    let request=TaskDomainLoadRequest {
                        query: Default::default(),
                        capture:resident_task::TaskCaptureRequest::current(),
                        record_limit:usize::try_from(limits.record_limit)?.min(65_536),
                        byte_limit:usize::try_from(limits.history_per_agent_bytes
                            .checked_add(limits.pending_per_agent_bytes).context("retained Stop request limits overflow")?)?
                            .min(64*1024*1024),
                    };
                    let finished=plan.finished_attempt()?;
                    let observed=bridge.read_finished_original_task_stop_attempt(&finished,&request).await?;
                    let Some(observed)=observed else {
                        self.inner.task_stop_effects.release_original_absence(&plan)?;
                        return Ok(());
                    };
                    let original=observed.shared_record();
                    anyhow::ensure!(original.prepared==*plan.prepared() && original.intent==*plan.intent(),
                        "retained Host TaskStop immutable source differs");
                    let source=ledger.register_task_stop_view(&scope,InputMemoryCategory::Pins,std::iter::once(&original))?;
                    return Box::pin(self.continue_original_task_stop_loaded_owned(original,source,request)).await;
                }
                // reserve_all reconciles the exact original witness/group;
                // no live SQL attempt can cross this owned reservation.
                let mutation = self.begin_mutation().await?;
                let (request, _request_material) = mutation.owner_task_load_request()?;
                let observed = self.inner.runtime_db.transaction(|tx| {
                    use rusqlite::OptionalExtension;
                    let seq:Option<u64> = tx.query_row(
                        "SELECT effect_seq FROM task_stop_effects WHERE command_nonce=?1 AND agent_id=?2 AND identity_incarnation=?3",
                        rusqlite::params![plan.prepared().command_nonce(),scope.agent_id,scope.identity_incarnation],
                        |r|r.get(0),
                    ).optional()?;
                    if let Some(seq) = seq {
                        return load_task_stop_by_sequence_tx(tx, &scope, seq, request.byte_limit, ledger).map(Some);
                    }
                    // Every successful initial Stop, including a canonical
                    // Task no-op, writes its effect and whole receipt together.
                    // Receipts remain unpruned; a finished attempt plus this
                    // fresh, reserved, joint absence proves no initial commit.
                    let receipt:bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM input_mutation_receipts WHERE command_nonce=?1)",
                        [plan.prepared().command_nonce()], |r|r.get(0),
                    )?;
                    anyhow::ensure!(!receipt,"original TaskStop whole receipt lost its mandatory effect");
                    Ok(None)
                })?;
                drop(mutation);
                let Some(observed) = observed else {
                    self.inner.task_stop_effects.release_original_absence(&plan)?;
                    return Ok(());
                };
                let original = observed.records.first().context("retained TaskStop exact source missing")?;
                anyhow::ensure!(original.prepared == *plan.prepared() && original.intent == *plan.intent(),
                    "retained TaskStop exact immutable source differs");
                let source = ledger.register_task_stop_view(&scope,InputMemoryCategory::Pins,std::iter::once(original))?;
                Box::pin(self.continue_original_task_stop_loaded_owned(original.clone(),source,request)).await
            }.await;
            if let Err(error) = result {
                tracing::warn!(original_nonce=%plan.prepared().command_nonce(),error=%error,
                    "retained original TaskStop remains recoverable");
                failed = true;
            }
        }
        anyhow::ensure!(!failed, "retained original TaskStop continuation failed");
        Ok(())
    }
}
