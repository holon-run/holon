//! Stop owns its frozen local plan before SQL and acknowledges before continuation.
use super::*;
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_task::{
        CanonicalTaskFact, QualifiedTaskFact, TaskCaptureRequest, TaskFamily, TaskKey,
    },
    task_stop_effects::PreparedTaskStopEffects,
    task_stop_intent::FrozenTaskStopIntent,
};
use crate::runtime_db::{
    task_stop::{TaskStopChildTarget, TaskStopRecord},
    transitions::{PreparedInputTransition, PreparedTaskTransition, TaskTransitionCommand},
};
use crate::runtime_error::RuntimeError;
use rusqlite::OptionalExtension;

struct StopChildObservation {
    target: Option<TaskStopChildTarget>,
    _material: Option<Arc<BudgetViewGuard>>,
}
struct AcceptedTaskStop {
    original: Arc<TaskStopRecord>,
    local: Arc<PreparedTaskStopEffects>,
    worker: task_stop_effects::OriginalTaskStopWorker,
    published: Option<task_stop_effects::PublishedTaskStopEffects>,
    _record_material: Arc<BudgetViewGuard>,
}
pub(super) struct OwnedTaskStopReply {
    pub(super) task: TaskRecord,
    _material: Arc<BudgetViewGuard>,
}

impl RuntimeHandle {
    /// Register before the first await; caller loss cannot detach continuation.
    pub(super) fn start_original_task_stop_owned(
        &self,
        task_id: &str,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<OwnedTaskStopReply>>> {
        self.start_original_task_stop_owned_with_initial_fault(
            task_id,
            self.take_transition_fault(),
        )
    }

    pub(super) fn start_original_task_stop_owned_with_initial_fault(
        &self,
        task_id: &str,
        initial_fault: Option<TransitionFaultPoint>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<OwnedTaskStopReply>>> {
        let nonce = crate::ids::audit_event_id();
        let bytes = serialized_bytes(&(task_id, &nonce, self.inner.mutation_owner.agent_id()))?
            .checked_mul(8)
            .and_then(|n| n.checked_add(1024))
            .context("owned TaskStop selector overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("owned TaskStop selector limits overflow")?
                    .min(64 * 1024 * 1024)
                && limits.record_limit >= 8,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "owned TaskStop selectors"
            )
        );
        let scope = resident_input_evidence::InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let selectors = self.inner.input_evidence_budget.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let task_id = task_id.to_owned();
        let runtime = self.clone();
        let (reply, received) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _selectors = selectors;
            let accepted = match runtime
                .prepare_task_stop_commit_with_fault(&task_id, nonce, initial_fault)
                .await
            {
                Ok(accepted) => accepted,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            // A confirmed initial NoHandle ACK can already have no pending
            // facet. It needs no signal capability and still returns that ACK.
            if !accepted.original.progress.is_pending() {
                let answer = (|| -> Result<OwnedTaskStopReply> {
                    anyhow::ensure!(
                        accepted.original.intent.signal
                            == crate::runtime_db::task_stop::TaskStopSignal::NoHandle
                            && accepted.original.intent.lease_id.is_none(),
                        "initial completed Stop unexpectedly owns a physical signal"
                    );
                    let bytes = serialized_bytes(&accepted.original.ack_task)?
                        .checked_mul(4)
                        .context("owned completed Stop reply overflow")?;
                    let material = runtime.inner.input_evidence_budget.register_owned_bytes(
                        &accepted.original.intent.task.scope,
                        InputMemoryCategory::Pins,
                        OwnedBytesEstimate {
                            payload_bytes: bytes,
                            metadata_estimate_bytes: 0,
                        },
                    )?;
                    accepted
                        .local
                        .finished
                        .store(true, std::sync::atomic::Ordering::Release);
                    runtime
                        .inner
                        .task_stop_effects
                        .release_finished(&accepted.local)?;
                    Ok(OwnedTaskStopReply {
                        task: accepted.original.ack_task.clone(),
                        _material: material,
                    })
                })();
                let _ = reply.send(answer);
                return;
            }
            let published = match accepted.published {
                Some(published) => published,
                None => {
                    let _ = reply.send(Err(anyhow::anyhow!(
                        "pending initial TaskStop omitted its original publication"
                    )));
                    return;
                }
            };
            // This capability has exact ACK/epoch/lease binding; no map lookup.
            published.signal_original();
            let answer = (|| -> Result<OwnedTaskStopReply> {
                let bytes = serialized_bytes(&accepted.original.ack_task)?
                    .checked_mul(4)
                    .context("owned Stop reply material overflow")?;
                let guard = runtime.inner.input_evidence_budget.register_owned_bytes(
                    &accepted.original.intent.task.scope,
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: bytes,
                        metadata_estimate_bytes: 0,
                    },
                )?;
                Ok(OwnedTaskStopReply {
                    task: accepted.original.ack_task.clone(),
                    _material: guard,
                })
            })();
            // An abandoned caller releases only its reply, never this worker.
            let _ = reply.send(answer);
            if let Err(error) = runtime
                .continue_published_task_stop_owned(
                    accepted.original.clone(),
                    published,
                    accepted.worker,
                )
                .await
            {
                tracing::warn!(task_id=%task_id, effect_seq=accepted.original.effect_seq,
                    error=%error, "original TaskStop continuation remains durable");
            }
        })?;
        Ok(received)
    }

    /// Result responsibility does not wait for a physical join or child cleanup.
    /// Sparse/retired routing is owned separately by the Host metadata actor.
    #[cfg(test)]
    async fn commit_original_task_stop_result(&self, original: &TaskStopRecord) -> Result<()> {
        let Some(plan) = &original.intent.result else {
            return Ok(());
        };
        let mut mutation = self.begin_mutation().await?;
        anyhow::ensure!(
            mutation.input_evidence_scope() == original.intent.task.scope,
            "TaskStop Result runtime differs from original owner"
        );
        let (request, _request_material) = mutation.owner_task_load_request()?;
        let prepared = PreparedInputTransition::task_stop_result(
            original,
            &request,
            &self.inner.input_evidence_budget,
        )?
        .with_task_stop_result_fault(self.take_transition_fault())?;
        let result = mutation.commit_current_task_stop_phase(prepared).await?;
        anyhow::ensure!(
            result.ack.original_nonce == original.prepared.command_nonce()
                && result.ack.binding == original.receipt_binding(),
            "TaskStop Result ACK differs from original Stop"
        );
        let actual = result
            .ack
            .result
            .as_ref()
            .context("TaskStop Result ACK omitted its actual outcome")?;
        anyhow::ensure!(
            actual.phase_nonce == plan.phase_nonce,
            "TaskStop Result ACK changed its original continuation address"
        );
        if let crate::runtime_db::task_stop::TaskStopResultOutcome::Delivered {
            message_id, ..
        } = &actual.outcome
        {
            anyhow::ensure!(
                message_id == &plan.message.id,
                "TaskStop Result ACK changed its original feedback identity"
            );
        }
        Ok(())
    }

    /// Discovery only: the original Task and child are rechecked under the final group.
    fn observe_stop_child_target(
        &self,
        qualified: &QualifiedTaskFact,
    ) -> Result<StopChildObservation> {
        let CanonicalTaskFact::Task(task) = &qualified.fact else {
            bail!("TaskStop child discovery requires original Task");
        };
        let child = task
            .detail
            .as_ref()
            .and_then(|d| d.get("child_agent_id"))
            .and_then(Value::as_str)
            .filter(|_| task.is_child_agent_task());
        let Some(child) = child else {
            return Ok(StopChildObservation {
                target: None,
                _material: None,
            });
        };
        let ledger = &self.inner.input_evidence_budget;
        self.inner.runtime_db.transaction(|tx| {
            let raw: u64 = tx.query_row(
                "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(kind AS BLOB))+length(CAST(visibility AS BLOB))+length(CAST(ownership AS BLOB))+length(CAST(status AS BLOB))+COALESCE(length(CAST(parent_agent_id AS BLOB)),0)+COALESCE(length(CAST(delegated_from_task_id AS BLOB)),0) FROM agent_identities WHERE agent_id=?1),0)",
                [child], |r| r.get(0),
            )?;
            let bytes = raw.checked_add(serialized_bytes(&(child, &task.id, &task.agent_id))?)
                .and_then(|n| n.checked_add(512)).and_then(|n| n.checked_mul(8))
                .context("TaskStop child discovery bytes overflow")?;
            let limits = ledger.limits();
            anyhow::ensure!(bytes <= limits.history_per_agent_bytes.checked_add(limits.pending_per_agent_bytes)
                .context("TaskStop child discovery limits overflow")?.min(64 * 1024 * 1024)
                && limits.record_limit >= 16,
                crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted("TaskStop child discovery"));
            let material = ledger.register_owned_bytes(&qualified.locator.scope, InputMemoryCategory::Candidate,
                OwnedBytesEstimate { payload_bytes: bytes, metadata_estimate_bytes: 0 })?;
            // No child State or body is returned. Missing/mismatched association grants no child target.
            let incarnation: Option<Option<u64>> = tx.query_row(
                "SELECT CASE WHEN json_valid(payload_json) THEN CASE WHEN kind='child' AND visibility='private' AND ownership='parent_supervised' AND status IN ('active','deleting','deleted') AND parent_agent_id=?2 AND delegated_from_task_id=?3 AND json_extract(payload_json,'$.agent_id')=agent_id AND json_extract(payload_json,'$.kind')=kind AND json_extract(payload_json,'$.visibility')=visibility AND json_extract(payload_json,'$.ownership')=ownership AND json_extract(payload_json,'$.status')=status AND json_extract(payload_json,'$.parent_agent_id')=parent_agent_id AND json_extract(payload_json,'$.delegated_from_task_id')=delegated_from_task_id THEN CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') END END END FROM agent_identities WHERE agent_id=?1",
                rusqlite::params![child, task.agent_id, task.id], |r| r.get(0),
            ).optional()?;
            let target = incarnation.flatten().map(|incarnation| TaskStopChildTarget {
                scope: resident_input_evidence::InputEvidenceScope { agent_id: child.to_owned(), identity_incarnation: incarnation },
                parent_agent_id: task.agent_id.clone(), creation_task_id: task.id.clone(),
            });
            Ok(StopChildObservation { target, _material: Some(material) })
        })
    }

    #[cfg(test)]
    async fn prepare_task_stop_commit(
        &self,
        task_id: &str,
        nonce: String,
    ) -> Result<AcceptedTaskStop> {
        Box::pin(self.prepare_task_stop_commit_with_fault(
            task_id,
            nonce,
            self.take_transition_fault(),
        ))
        .await
    }

    async fn prepare_task_stop_commit_with_fault(
        &self,
        task_id: &str,
        nonce: String,
        initial_fault: Option<TransitionFaultPoint>,
    ) -> Result<AcceptedTaskStop> {
        let ledger = &self.inner.input_evidence_budget;
        let bytes = serialized_bytes(&(task_id, &nonce, self.inner.mutation_owner.agent_id()))?
            .checked_mul(8)
            .and_then(|n| n.checked_add(1024))
            .context("TaskStop original selector bytes overflow")?;
        let limits = ledger.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("TaskStop selector limits overflow")?
                    .min(64 * 1024 * 1024)
                && limits.record_limit >= 8,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "TaskStop original selectors"
            )
        );
        let scope = resident_input_evidence::InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let _selectors = ledger.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let request = TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            recovery_source_ids: Default::default(),
            required_keys: std::collections::BTreeSet::from([TaskKey::new(
                TaskFamily::Task,
                task_id,
            )]),
        };
        let mut first = self.begin_mutation().await?;
        first.prepare_owner_task_request(&request).await?;
        let mut original = first.capture_tasks(&request)?;
        let key = TaskKey::new(TaskFamily::Task, task_id);
        let original_binding = {
            let qualified = original.lookup(&key)?.ok_or_else(|| {
                RuntimeError::validation(
                    "task_not_running",
                    format!("task {task_id} is not currently running"),
                )
                .with_safe_context("task_id", task_id)
            })?;
            let CanonicalTaskFact::Task(task) = &qualified.fact else {
                bail!("TaskStop original family differs");
            };
            let lease = self
                .inner
                .task_handles
                .lock()
                .await
                .get(task_id)
                .and_then(command_task::ManagedTaskHandle::control_lease);
            match lease {
                Some(lease) => match lease.original_child()? {
                    Some(source) if !source.monitor_binding_confirmed() => {
                        anyhow::ensure!(
                            lease.matches_task_generation(task)
                                && source.address().scope == first.input_evidence_scope(),
                            "TaskStop binding belongs to another original producer generation"
                        );
                        Some((lease, source))
                    }
                    _ => None,
                },
                None => None,
            }
        };
        let coordinated_lease = if let Some((lease, source)) = original_binding {
            // A finite spawn and binding may acquire this same parent owner.
            // Keep only the original producer capability across that boundary.
            drop(original);
            drop(first);
            self.continue_original_child_spawn(source.clone()).await?;
            if source.observed_terminal().is_some() {
                self.continue_original_child_terminal(source.clone())
                    .await?;
                return Err(RuntimeError::validation(
                    "task_not_running",
                    format!("task {task_id} already reached its original terminal result"),
                )
                .with_safe_context("task_id", task_id)
                .into());
            }
            anyhow::ensure!(
                source.monitor_stage_present(),
                "TaskStop original spawn has no actual monitor binding stage"
            );
            anyhow::ensure!(
                self.continue_original_child_monitor_binding(source).await?,
                "TaskStop original monitor binding remains unavailable"
            );
            first = self.begin_mutation().await?;
            first.prepare_owner_task_request(&request).await?;
            original = first.capture_tasks(&request)?;
            Some(lease)
        } else {
            None
        };
        let qualified = original.lookup(&key)?.ok_or_else(|| {
            RuntimeError::validation(
                "task_not_running",
                format!("task {task_id} is not currently running"),
            )
            .with_safe_context("task_id", task_id)
        })?;
        let child = self.observe_stop_child_target(qualified)?;
        drop(first);
        // Release the parent before acquiring the complete sorted original group.
        let mut mutation = self
            .begin_task_stop_with_readonly_child(child.target.as_ref())
            .await?;
        mutation.prepare_owner_task_request(&request).await?;
        let selected = mutation.capture_tasks(&request)?;
        let current = selected
            .lookup(&key)?
            .context("TaskStop original Task disappeared")?;
        anyhow::ensure!(
            current.locator == qualified.locator,
            crate::runtime_db::RuntimeStateTransitionConflict::concurrent_mutation("task", task_id)
        );
        let CanonicalTaskFact::Task(task) = &current.fact else {
            bail!("TaskStop original family differs")
        };
        let handles = self.inner.task_handles.lock().await;
        let handle = handles.get(task_id);
        if let Some(original_lease) = coordinated_lease.as_ref() {
            anyhow::ensure!(
                handle
                    .and_then(command_task::ManagedTaskHandle::control_lease)
                    .is_some_and(|current| Arc::ptr_eq(&current, original_lease)),
                crate::runtime_db::RuntimeStateTransitionConflict::concurrent_mutation(
                    "task", task_id
                )
            );
        }
        if handle.is_none() && task.kind != TaskKind::CommandTask {
            anyhow::ensure!(
                task.status == TaskStatus::Interrupted
                    && task.is_child_agent_task()
                    && task
                        .detail
                        .as_ref()
                        .and_then(|d| d.get("child_agent_id"))
                        .and_then(Value::as_str)
                        .is_some(),
                RuntimeError::validation(
                    "task_not_running",
                    format!("task {task_id} is not currently running")
                )
                .with_safe_context("task_id", task_id)
            );
        }
        let frozen = FrozenTaskStopIntent::prepare(
            current,
            handle,
            self.inner.mutation_owner.owner_epoch(),
            child.target.as_ref(),
            self.now(),
            ledger,
        )?;
        let command = TaskTransitionCommand {
            agent_id: task.agent_id.clone(),
            expected_owner_incarnation: self.inner.identity_incarnation,
            task: frozen.task.clone(),
            agent_deletion: None,
            task_result_settlement: None,
            queue_entry: None,
            work_items: vec![],
            expected_wait_conditions: vec![],
            wait_conditions: vec![],
            agent_state: None,
            message_evidence: vec![],
            audit_events: vec![AuditEvent::legacy(
                "task_status_updated",
                serde_json::json!({"task_id":task.id,"task_status":frozen.task.status}),
            )],
            index_changes: vec![],
            notify_scheduler: true,
            commit_on_idempotent: false,
            fault: initial_fault,
        };
        let prepared = PreparedInputTransition::task(
            frozen.intent.task.scope.clone(),
            nonce,
            frozen.task.updated_at,
            PreparedTaskTransition::new(command, Default::default(), false, None, None)?
                .with_stop_intent(frozen.intent.clone())?,
        )?;
        let local = PreparedTaskStopEffects::freeze(
            prepared.descriptor(),
            &frozen.intent,
            task,
            handle,
            ledger,
        )?;
        let worker = local
            .claim_worker()
            .context("new initial TaskStop worker already claimed")?;
        self.inner
            .task_stop_effects
            .retain_before_sql(local.clone())?;
        drop(handles);
        let initial = mutation.commit_initial_task_stop(prepared).await;
        local.mark_sql_finished();
        let ack = match initial {
            Ok(ack) => ack,
            Err(error) => {
                if crate::runtime_db::persistence_certainty(&error)
                    == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
                {
                    // The synchronous original attempt ended in a proven rollback.
                    self.inner
                        .task_stop_effects
                        .release_original_absence(&local)?;
                }
                return Err(error);
            }
        };
        let capture = mutation.capture_tasks(&request)?;
        let original = capture
            .stop_head(task_id)?
            .context("TaskStop ACK omitted original head")?;
        anyhow::ensure!(
            original.receipt_binding() == ack.binding,
            "TaskStop installed head differs from original ACK"
        );
        let material = ledger.register_task_stop_view(
            &original.intent.task.scope,
            InputMemoryCategory::Pins,
            std::iter::once(original),
        )?;
        let original = original.clone();
        // Mint while the same owner still holds the complete initial ACK.
        // A later owner replacement cannot turn an acknowledged Stop into an
        // error merely by removing it from the current pending inventory.
        let published = if original.progress.is_pending() {
            Some(self.inner.task_stop_effects.confirm_published(
                &self.inner.mutation_owner,
                &original,
                ledger,
            )?)
        } else {
            None
        };
        Ok(AcceptedTaskStop {
            original,
            local,
            worker,
            published,
            _record_material: material,
        })
    }
}

#[cfg(test)]
mod initial_tests {
    use super::*;
    use crate::provider::StubProvider;
    use crate::runtime::{
        command_task::CommandTaskHandle, control_cancellation::SharedControlSignal,
    };
    use tokio::sync::{mpsc, oneshot};

    #[tokio::test]
    async fn original_initial_worker_claim_prevents_a_sweep_from_completing_before_ack_reply(
    ) -> Result<()> {
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
            id: "task:initial-sweep-window".into(),
            agent_id: "default".into(),
            kind: TaskKind::ActorInvocation,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({})),
            recovery: None,
        };
        runtime
            .persist_task_transition(&task, "initial_sweep_window_seed")
            .await?;
        let lease = runtime.inner.control_cancellations.task(&task);
        lease.attach(tokio::spawn(std::future::pending::<()>()));
        runtime.inner.task_handles.lock().await.insert(
            task.id.clone(),
            ManagedTaskHandle::Async(control_cancellation::SharedControlTaskHandle(lease)),
        );
        // Pause the actual initial producer after SQL/publication and before
        // its public reply. An owned sweep must coalesce with its held claim.
        let accepted = runtime
            .prepare_task_stop_commit(&task.id, "initial-sweep-window".into())
            .await?;
        runtime.continue_retained_task_stop_plans_owned().await?;
        let (physical,result,messages):(String,String,u64)=runtime.runtime_db().connection()?.query_row(
            "SELECT physical_state,result_state,(SELECT COUNT(*) FROM messages WHERE kind='task_result') FROM task_stop_effects WHERE task_id=?1",
            [&task.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(
            (physical, result, messages),
            ("pending".into(), "pending".into(), 0)
        );
        assert_eq!(accepted.original.ack_task.status, TaskStatus::Cancelled);
        let published = accepted
            .published
            .context("initial ACK must retain its actual publication capability")?;
        runtime
            .continue_published_task_stop_owned(
                accepted.original.clone(),
                published,
                accepted.worker,
            )
            .await?;
        let (physical,result,messages):(String,String,u64)=runtime.runtime_db().connection()?.query_row(
            "SELECT physical_state,result_state,(SELECT COUNT(*) FROM messages WHERE kind='task_result') FROM task_stop_effects WHERE task_id=?1",
            [&task.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(
            (physical, result, messages),
            ("monitor_joined".into(), "completed".into(), 1)
        );
        Ok(())
    }

    #[tokio::test]
    async fn owned_initial_no_handle_stop_returns_confirmed_ack_without_a_pending_signal_capability(
    ) -> Result<()> {
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
            id: "task:owned-no-handle".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
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
            .persist_task_transition(&task, "owned_no_handle_seed")
            .await?;
        let response = runtime.start_original_task_stop_owned(&task.id)?.await??;
        assert_eq!(response.task.status, TaskStatus::Cancelled);
        drop(response);
        runtime.join_owned_mutations().await?;
        let (physical, result): (String, String) = runtime.runtime_db().connection()?.query_row(
            "SELECT physical_state,result_state FROM task_stop_effects WHERE task_id=?1",
            [&task.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(
            (physical, result),
            ("missing_proof".into(), "not_required".into())
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.pending.count,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn owned_stop_registration_outlives_abandoned_caller_and_closed_gate_prevents_sql(
    ) -> Result<()> {
        for closed in [false, true] {
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
                id: "task:owned-stop-abandoned".into(),
                agent_id: "default".into(),
                kind: TaskKind::ActorInvocation,
                status: TaskStatus::Running,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: None,
                detail: Some(serde_json::json!({})),
                recovery: None,
            };
            runtime
                .persist_task_transition(&task, "owned_stop_abandoned_seed")
                .await?;
            let lease = runtime.inner.control_cancellations.task(&task);
            lease.attach(tokio::spawn(std::future::pending::<()>()));
            runtime.inner.task_handles.lock().await.insert(
                task.id.clone(),
                ManagedTaskHandle::Async(control_cancellation::SharedControlTaskHandle(
                    lease.clone(),
                )),
            );
            if closed {
                runtime.close_mutation_admission();
            }
            let result = runtime.start_original_task_stop_owned(&task.id);
            if closed {
                assert!(result.is_err());
            } else {
                drop(result?);
                runtime.join_owned_mutations().await?;
            }
            let (stops,messages):(u64,u64)=runtime.runtime_db().connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM task_stop_effects),(SELECT COUNT(*) FROM messages WHERE kind='task_result')",
                [],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!((stops, messages), if closed { (0, 0) } else { (1, 1) });
            assert_eq!(
                runtime.inner.input_evidence_budget.report()?.pending.count,
                if closed { 0 } else { 1 }
            );
            if !closed {
                lease.join().await?;
            } else {
                lease.cancel();
                lease.join().await?;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn original_result_runtime_actor_survives_rollback_and_lost_publication_without_join_or_duplicate_input(
    ) -> Result<()> {
        for fault in [
            Some(TransitionFaultPoint::BeforeCommit),
            Some(TransitionFaultPoint::BeforeCacheUpdate),
            None,
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
                id: "task:original-result-actor".into(),
                agent_id: "default".into(),
                kind: TaskKind::ActorInvocation,
                status: TaskStatus::Running,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: None,
                detail: Some(serde_json::json!({})),
                recovery: None,
            };
            runtime
                .persist_task_transition(&task, "original_result_actor_seed")
                .await?;
            let lease = runtime.inner.control_cancellations.task(&task);
            lease.attach(tokio::spawn(std::future::pending::<()>()));
            runtime.inner.task_handles.lock().await.insert(
                task.id.clone(),
                ManagedTaskHandle::Async(control_cancellation::SharedControlTaskHandle(
                    lease.clone(),
                )),
            );
            let accepted = runtime
                .prepare_task_stop_commit(&task.id, "original-result-stop".into())
                .await?;
            let frozen = accepted
                .original
                .intent
                .result
                .as_ref()
                .context("noncommand feedback missing")?;
            if let Some(fault) = fault {
                runtime.inject_next_transition_fault(fault);
            }
            let first = runtime
                .commit_original_task_stop_result(&accepted.original)
                .await;
            assert!(
                !lease.is_finished(),
                "Result delivery must not join or abort the original monitor"
            );
            let count = || -> Result<u64> {
                Ok(runtime.runtime_db().connection()?.query_row(
                    "SELECT COUNT(*) FROM messages WHERE message_id=?1",
                    [&frozen.message.id],
                    |r| r.get(0),
                )?)
            };
            if fault == Some(TransitionFaultPoint::BeforeCommit) {
                assert!(first.is_err());
                assert_eq!(count()?, 0);
                assert_eq!(
                    runtime.inner.input_evidence_budget.report()?.pending.count,
                    0
                );
            } else if fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
                assert!(first.is_err());
                assert_eq!(count()?, 1);
                assert_eq!(
                    runtime.inner.input_evidence_budget.report()?.pending.count,
                    1
                );
                runtime.reconcile_posture_observation().await?;
            } else {
                first?;
                assert_eq!(count()?, 1);
            }
            runtime
                .commit_original_task_stop_result(&accepted.original)
                .await?;
            runtime
                .commit_original_task_stop_result(&accepted.original)
                .await?;
            assert_eq!(count()?, 1);
            let report = runtime.inner.input_evidence_budget.report()?;
            assert_eq!(report.pending.count, 1);
            assert_eq!(report.pending.unresolved_count, 0);
            let progress: (String, String) = runtime.runtime_db().connection()?.query_row(
                "SELECT physical_state,result_state FROM task_stop_effects WHERE effect_seq=?1",
                [accepted.original.effect_seq],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(progress, ("pending".into(), "completed".into()));
            lease.cancel();
            lease.join().await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn initial_stop_actor_does_not_signal_before_ack_or_publication_and_keeps_known_plan(
    ) -> Result<()> {
        for fault in [
            Some(TransitionFaultPoint::BeforeCommit),
            Some(TransitionFaultPoint::BeforeCacheUpdate),
            None,
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
                id: "task:owned-stop-original".into(),
                agent_id: "default".into(),
                kind: TaskKind::CommandTask,
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
                .persist_task_transition(&task, "owned_stop_seed")
                .await?;
            let (cancel, mut cancel_rx) = oneshot::channel();
            let (force, mut force_rx) = oneshot::channel();
            let cancel = SharedControlSignal::new(cancel);
            let force = SharedControlSignal::new(force);
            let lease =
                runtime
                    .inner
                    .control_cancellations
                    .command(&task, cancel.clone(), force.clone());
            let (input_tx, _) = mpsc::channel(1);
            runtime.inner.task_handles.lock().await.insert(
                task.id.clone(),
                ManagedTaskHandle::Command(CommandTaskHandle {
                    cancel_tx: Some(cancel),
                    force_stop_tx: Some(force),
                    input_tx,
                    control_lease: Some(lease),
                }),
            );
            if let Some(fault) = fault {
                runtime.inject_next_transition_fault(fault);
            }
            let result = runtime
                .prepare_task_stop_commit(&task.id, "owned-stop-original".into())
                .await;
            assert!(matches!(
                cancel_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            assert!(matches!(
                force_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            if fault == Some(TransitionFaultPoint::BeforeCommit) {
                assert!(result.is_err());
                assert_eq!(
                    runtime
                        .runtime_db()
                        .tasks()
                        .latest(&task.id)?
                        .unwrap()
                        .status,
                    TaskStatus::Running
                );
                let count: u64 = runtime.runtime_db().connection()?.query_row(
                    "SELECT COUNT(*) FROM task_stop_effects WHERE command_nonce='owned-stop-original'", [], |r| r.get(0),
                )?;
                assert_eq!(count, 0);
                assert!(runtime.inner.mutation_owner.capture().is_ok());
                continue;
            }
            if fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
                assert!(result.is_err());
                assert!(runtime.inner.mutation_owner.capture().is_err());
                runtime.reconcile_posture_observation().await?;
            } else {
                let accepted = result?;
                assert_eq!(accepted.original.ack_task.status, TaskStatus::Cancelling);
                assert!(!accepted.local.finished.load(Ordering::Acquire));
            }
            let root = runtime
                .inner
                .mutation_owner
                .capture()?
                .context("initial Stop not published")?;
            let captured = root.tasks.as_ref().unwrap().capture_pending_stops()?;
            let record = captured
                .pending_stops()?
                .find(|r| r.prepared.command_nonce() == "owned-stop-original")
                .context("original Stop responsibility not published")?;
            let permitted = runtime.inner.task_stop_effects.confirm_published(
                &runtime.inner.mutation_owner,
                record,
                &runtime.inner.input_evidence_budget,
            )?;
            permitted.signal_original();
            assert!(cancel_rx.try_recv().is_ok());
            permitted.signal_original();
            assert!(matches!(
                force_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
        }
        Ok(())
    }
}
