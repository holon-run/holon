//! Explicit Task preparation under the original Ready owner reservation.

use super::*;
use crate::{
    runtime::resident_task::{
        CapturedTasks, CommittedTaskDelta, TaskCaptureRequest, TaskReadError,
    },
    runtime_db::{
        context_inputs::ContextDurableRead,
        task_snapshot::{self, TaskDomainLoadRequest},
    },
};

pub(in crate::runtime) struct OwnerTaskRequestMaterial {
    _queries: super::super::task_queries::TaskQuerySnapshot,
    _mandatory: Arc<super::super::input_evidence_budget::BudgetViewGuard>,
}

impl RuntimeMutation {
    /// Completed reply ownership and live Work use one final publication.
    pub(in crate::runtime) fn capture_reply_work_owner(
        &self,
        lease: &super::super::task_queries::TaskQueryLease,
        message_id: &str,
    ) -> Result<(
        Option<String>,
        Arc<super::super::input_evidence_budget::BudgetViewGuard>,
    )> {
        let root = self.capture_published_root()?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        let work = root
            .work
            .as_ref()
            .ok_or(super::super::resident_work::WorkReadError::IncompleteLive)?;
        anyhow::ensure!(
            tasks.scope() == &self.input_evidence_scope() && work.scope() == tasks.scope(),
            "reply Task/Work publication scope differs"
        );
        self.runtime.inner.task_queries.validate_lease(
            lease,
            tasks.scope(),
            root.token.owner_epoch,
        )?;
        anyhow::ensure!(
            lease.selection().reply_message_ids.contains(message_id),
            "reply query role differs"
        );
        let tasks = tasks.capture_query(lease.selection())?;
        let work = work.capture(&super::super::work_reads::live_work_request())?;
        let owner = super::super::task_reads::reply_work_owner(&tasks, &work, message_id)?;
        let (_, _records, _bytes) = self.context_input_evidence_policy()?;
        let estimate = super::super::input_evidence_budget::serialized_bytes(&owner)?
            .checked_add(128)
            .and_then(|n| n.checked_mul(8))
            .context("reply Work owner bytes overflow")?;

        let guard = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &tasks.scope,
                super::super::input_evidence_budget::InputMemoryCategory::Pins,
                super::super::input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: estimate,
                },
            )?;
        Ok((owner.map(str::to_owned), guard))
    }

    /// Materialization choice only; the command's frozen business digest is unchanged.
    pub(in crate::runtime) fn owner_task_load_request(
        &self,
    ) -> Result<(TaskDomainLoadRequest, OwnerTaskRequestMaterial)> {
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let scope = self.input_evidence_scope();
        let coverage = self.tasks.as_ref().map(|tasks| tasks.coverage());
        anyhow::ensure!(
            self.tasks
                .as_ref()
                .is_none_or(|tasks| tasks.scope() == &scope),
            "owner Task request scope differs"
        );
        let queries = self.runtime.inner.task_queries.snapshot(
            &scope,
            self.reservations[self.primary_index].owner_epoch(),
            &self.runtime.inner.input_evidence_budget,
            record_limit,
            byte_limit,
        )?;
        let activation = self
            .base
            .state
            .current_execution_binding
            .as_ref()
            .and_then(|binding| binding.activation_id.as_deref());
        let ids = activation.and_then(|activation| {
            coverage.and_then(|coverage| coverage.activation_result_ids.get(activation))
        });
        for id in ids.into_iter().flatten() {
            let coverage = coverage.context("current Task activation has no domain")?;
            let metadata = coverage
                .result_metadata
                .get(id)
                .context("current Task activation metadata missing")?;
            anyhow::ensure!(metadata.activation_id.as_deref() == activation
                && metadata.state == crate::runtime_db::task_result_settlement::TaskResultSettlementState::CallerAdmitted
                && coverage.unsettled_complete && coverage.unsettled_result_ids.contains(id),
                "current Task activation differs from complete unsettled metadata");
        }
        let count = ids.map_or(0, BTreeSet::len);
        let metadata = super::super::input_evidence_budget::serialized_bytes(&ids)?
            .checked_add(
                u64::try_from(count)?
                    .checked_mul(256)
                    .context("current Task selector nodes overflow")?,
            )
            .and_then(|bytes| bytes.checked_add(128))
            .and_then(|bytes| bytes.checked_mul(4))
            .context("current Task selector bytes overflow")?;

        let mandatory = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &scope,
                super::super::input_evidence_budget::InputMemoryCategory::Candidate,
                super::super::input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: metadata,
                },
            )?;
        // The prior coverage's required_keys are a past observation, not an
        // immortal query role. Current active/unsettled frontiers stay complete.
        let mut required_keys = queries.selection.required_keys.clone();
        required_keys.extend(ids.into_iter().flatten().map(|id| {
            crate::runtime::resident_task::TaskKey::new(
                crate::runtime::resident_task::TaskFamily::ResultSettlement,
                id,
            )
        }));
        Ok((
            TaskDomainLoadRequest {
                query: queries.selection.clone(),
                capture: TaskCaptureRequest {
                    required_keys,
                    ..TaskCaptureRequest::current()
                },
                recovery_at: None,
                record_limit,
                byte_limit,
            },
            OwnerTaskRequestMaterial {
                _queries: queries,
                _mandatory: mandatory,
            },
        ))
    }

    pub(in crate::runtime) async fn prepare_owner_task_query(
        &mut self,
        lease: &super::super::task_queries::TaskQueryLease,
    ) -> Result<()> {
        self.runtime.inner.task_queries.validate_lease(
            lease,
            &self.input_evidence_scope(),
            self.reservations[self.primary_index].owner_epoch(),
        )?;
        let (load, _material) = self.owner_task_load_request()?;
        self.prepare_owner_task_domain_request_inner(&load, "query")
            .await
    }

    pub(super) fn install_committed_task_delta(
        &mut self,
        delta: &CommittedTaskDelta,
    ) -> Result<()> {
        anyhow::ensure!(
            delta.scope == self.input_evidence_scope(),
            "committed Task scope differs"
        );
        // A delta has no authority to establish missing completeness or bodies.
        // Even a full-looking ACK needs explicit preparation when the old domain is absent.
        if let Some(tasks) = &self.tasks {
            self.tasks = Some(Arc::new(tasks.with_committed(delta)?));
        }
        Ok(())
    }

    // Activated by the D5 Task consumer slice; capture itself never prepares.
    pub(in crate::runtime) fn capture_tasks(
        &self,
        request: &TaskCaptureRequest,
    ) -> Result<CapturedTasks> {
        let root = self.capture_published_root()?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            tasks.scope() == &self.input_evidence_scope(),
            "Task capture scope differs"
        );
        tasks.capture(request)
    }

    pub(in crate::runtime) fn capture_tasks_with_state(
        &self,
        request: &TaskCaptureRequest,
    ) -> Result<(
        super::super::mutation_owner::PublicationToken,
        Arc<AgentState>,
        CapturedTasks,
    )> {
        let root = self.capture_published_root()?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            tasks.scope() == &self.input_evidence_scope(),
            "Task capture scope differs"
        );
        Ok((root.token, root.state.clone(), tasks.capture(request)?))
    }

    #[inline(never)]
    pub(in crate::runtime) fn prepare_owner_task_request<'a>(
        &'a mut self,
        request: &'a TaskCaptureRequest,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.prepare_owner_task_request_inner(request))
    }

    async fn prepare_owner_task_request_inner(
        &mut self,
        request: &TaskCaptureRequest,
    ) -> Result<()> {
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let load = TaskDomainLoadRequest {
            query: Default::default(),
            capture: TaskCaptureRequest {
                required_keys: request.required_keys.clone(),
                recovery_source_ids: request.recovery_source_ids.clone(),
                ..TaskCaptureRequest::current()
            },
            recovery_at: None,
            record_limit,
            byte_limit,
        };
        self.prepare_owner_task_domain_request_inner(&load, "owner")
            .await
    }

    #[inline(never)]
    pub(in crate::runtime) fn prepare_context_tasks<'a>(
        &'a mut self,
        request: &'a TaskCaptureRequest,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Context derives exact settlement keys from this activation's
            // complete unsettled metadata. Only the requested bodies are captured.
            match self.capture_tasks(request) {
                Ok(tasks) if tasks.coverage.unsettled_complete => return Ok(()),
                Ok(_) => {}
                Err(error) if error.downcast_ref::<TaskReadError>().is_some() => {}
                Err(error) => return Err(error),
            }
            self.prepare_dispatch_tasks(request, None).await
        })
    }

    pub(in crate::runtime) async fn prepare_recovery_tasks(
        &mut self,
        request: &TaskCaptureRequest,
    ) -> Result<()> {
        let root = self.capture_published_root()?;
        if let Some(tasks) = &root.tasks {
            anyhow::ensure!(
                tasks.scope() == &self.input_evidence_scope(),
                "published Task scope differs"
            );
            match tasks.capture(request) {
                Ok(_) => return Ok(()),
                Err(error) if error.downcast_ref::<TaskReadError>().is_some() => {}
                Err(error) => return Err(error),
            }
        }
        drop(root);
        let (mut load, _live_queries) = self.owner_task_load_request()?;
        load.capture.terminal_limit = self
            .tasks
            .as_ref()
            .map_or(0, |tasks| tasks.coverage().terminal.limit);
        load.capture
            .required_keys
            .extend(request.required_keys.iter().cloned());
        let _stage = crate::diagnostics::attribution::TASK_RESULT_RECOVERY_TASK_PREPARE.start();
        self.prepare_owner_task_domain_request_inner(&load, "recovery")
            .await
    }

    #[inline(never)]
    pub(in crate::runtime) fn prepare_dispatch_tasks<'a>(
        &'a mut self,
        request: &'a TaskCaptureRequest,
        recovery_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let (mut load, _live_queries) = self.owner_task_load_request()?;
            load.capture
                .required_keys
                .extend(request.required_keys.iter().cloned());
            load.capture
                .recovery_source_ids
                .clone_from(&request.recovery_source_ids);
            load.recovery_at = recovery_at;
            // Dispatch needs complete active and unsettled metadata for its
            // current activation. Retain any already published history window.
            load.capture.terminal_limit = request.terminal_limit.max(
                self.tasks
                    .as_ref()
                    .map_or(0, |tasks| tasks.coverage().terminal.limit),
            );
            self.prepare_owner_task_domain_request_inner(&load, "dispatch")
                .await
        })
    }

    #[inline(never)]
    pub(in crate::runtime) fn prepare_owner_task_domain_request<'a>(
        &'a mut self,
        load: &'a TaskDomainLoadRequest,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.prepare_owner_task_domain_request_inner(load, "explicit"))
    }

    async fn prepare_owner_task_domain_request_inner(
        &mut self,
        load: &TaskDomainLoadRequest,
        caller: &'static str,
    ) -> Result<()> {
        use crate::diagnostics::attribution;

        let _attempt = attribution::TASK_PREPARE_ATTEMPT.start();
        let root = self.capture_published_root()?;
        let scope = self.input_evidence_scope();
        let (_, _record_limit, _byte_limit) = self.context_input_evidence_policy()?;

        anyhow::ensure!(
            load.capture.recovery_source_ids.is_empty() || load.recovery_at.is_some(),
            "Task recovery selectors require a frozen recovery_at"
        );
        let mut miss = "domain";
        if let Some(tasks) = &root.tasks {
            anyhow::ensure!(tasks.scope() == &scope, "published Task scope differs");
            match tasks
                .coverage()
                .recovery_sources_prepared_at(&load.capture.recovery_source_ids, load.recovery_at)
                .and_then(|_| tasks.capture_query(&load.query))
                .and_then(|_| tasks.capture(&load.capture))
            {
                Ok(_) => {
                    let _reuse = attribution::TASK_PREPARE_REUSE.start();
                    tracing::trace!(target: "holon::performance", caller, outcome = "reuse",
                        "Task preparation");
                    return Ok(());
                }
                Err(error) => match error.downcast_ref::<TaskReadError>() {
                    Some(TaskReadError::RecoveryTimeNotPrepared(_)) => {
                        miss = "recovery_time";
                    }
                    Some(_) => {
                        miss = "coverage";
                    }
                    None => return Err(error),
                },
            }
        }
        let _miss = match miss {
            "domain" => attribution::TASK_PREPARE_MISS_DOMAIN.start(),
            "coverage" => attribution::TASK_PREPARE_MISS_COVERAGE.start(),
            _ => attribution::TASK_PREPARE_MISS_RECOVERY_TIME.start(),
        };
        tracing::trace!(target: "holon::performance", caller, miss, outcome = "load",
            "Task preparation");
        let tasks = self.runtime.inner.runtime_db.transaction(|tx| {
            let _span = tracing::trace_span!(
                "task_domain_prepare",
                agent_id = %scope.agent_id,
                terminal_limit = load.capture.terminal_limit,
                required_keys = load.capture.required_keys.len(),
                recovery_sources = load.capture.recovery_source_ids.len(),
            )
            .entered();
            let _stage = crate::diagnostics::attribution::TASK_DOMAIN_PREPARE.start();
            let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
            anyhow::ensure!(
                facts.state.as_deref() == Some(root.state.as_ref())
                    && facts.control_revision.is_some(),
                "Task preparation canonical State differs from published root"
            );
            let identity = facts
                .identity
                .as_ref()
                .context("Task preparation identity missing")?;
            anyhow::ensure!(
                identity.agent_id == scope.agent_id
                    && identity.incarnation == scope.identity_incarnation,
                "Task preparation incarnation differs"
            );
            let prepared = if identity.status == crate::types::AgentRegistryStatus::Deleting {
                task_snapshot::prepare_original_task_recovery_snapshot_with_budget_tx(
                    tx,
                    &scope,
                    load,
                    &self.runtime.inner.input_evidence_budget,
                )?
            } else {
                task_snapshot::prepare_task_domain_snapshot_with_budget_tx(
                    tx,
                    &scope,
                    load,
                    &self.runtime.inner.input_evidence_budget,
                )?
            }
            .require_current()?;
            anyhow::ensure!(
                prepared.facts.identity == facts.identity
                    && prepared.facts.state == facts.state
                    && prepared.facts.control_revision == facts.control_revision,
                "Task preparation domains differ from the same read view"
            );
            Ok(prepared.resident)
        })?;
        let current = self.reservations[self.primary_index]
            .capture_ready_snapshot()?
            .context("Task preparation lost its published root")?;
        anyhow::ensure!(
            current.token == root.token
                && current.state == root.state
                && current.token.owner_epoch == self.reservations[self.primary_index].owner_epoch(),
            "Task preparation reservation/publication generation differs"
        );
        self.tasks = Some(Arc::new(tasks));
        let facts = Arc::new(self.facts(Default::default()));
        let reservation = &mut self.reservations[self.primary_index];
        reservation.begin(crate::ids::audit_event_id())?;
        MutationReservation::committed_all(
            std::slice::from_mut(reservation),
            vec![KnownCommit::State {
                state: facts.state.clone(),
                episodes: facts.episodes.clone(),
                input_evidence: facts.input_evidence.clone(),
                work: facts.work.clone(),
                tasks: facts.tasks.clone(),
                scheduling: facts.scheduling.clone(),
                runtime: Some(facts.clone()),
            }],
        )?;
        // Qualification ACK is saved before the first await; it is not a business commit.
        self.runtime.install_mutation_facts(&facts).await?;
        for observation in MutationReservation::publish_all_known(std::slice::from_mut(
            &mut self.reservations[self.primary_index],
        ))?
        .into_iter()
        .flatten()
        {
            trace_publication(observation.token);
        }
        self.refresh_baseline()?;
        Ok(())
    }
}

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        resident_task::{TaskFamily, TaskKey},
        tests::support::{context_config, tempdir, StubProvider},
    };

    #[tokio::test]
    async fn dispatch_reuses_recovery_sources_after_clock_advance_without_loading_tasks(
    ) -> Result<()> {
        let home = tempdir()?;
        let runtime = RuntimeHandle::new(
            "clock-task-owner",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "clock-task-owner".into(),
            context_config(),
        )?;
        let source = runtime
            .enqueue(MessageEnvelope::new(
                "clock-task-owner",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "unchanged recovery source".into(),
                },
            ))
            .await?;
        let mut mutation = runtime.begin_mutation().await?;
        let request = TaskCaptureRequest {
            recovery_source_ids: std::collections::BTreeSet::from([source.id]),
            ..TaskCaptureRequest::current()
        };
        let at = runtime.now();
        mutation.prepare_dispatch_tasks(&request, Some(at)).await?;
        let prepared = mutation.capture_published_root()?;
        let coverage = prepared.tasks.as_ref().unwrap().coverage();
        let observation = &coverage.recovery_sources[request.recovery_source_ids.first().unwrap()];
        assert!(observation.time_independent);
        let mut mixed = coverage.clone();
        for anchor in [serde_json::Value::Null, serde_json::json!("legacy-anchor")] {
            let mut legacy = serde_json::to_value(observation)?;
            legacy.as_object_mut().unwrap().remove("time_independent");
            legacy["paused_replays"] = serde_json::json!({
                "legacy-result": {
                    "recovery_epoch": 1, "root_is_charged": true,
                    "interrupted_anchor": anchor
                }
            });
            mixed
                .recovery_sources
                .insert("legacy".into(), serde_json::from_value(legacy)?);
            let sources = mixed.recovery_sources.keys().cloned().collect();
            mixed.recovery_sources_prepared_at(&sources, Some(at))?;
            let error = mixed
                .recovery_sources_prepared_at(&sources, Some(at + chrono::Duration::seconds(1)))
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<TaskReadError>(),
                Some(TaskReadError::RecoveryTimeNotPrepared(_))
            ));
        }
        let mut malformed = mixed.recovery_sources["legacy"].clone();
        malformed.time_independent = true;
        mixed.recovery_sources.insert("legacy".into(), malformed);
        assert!(mixed
            .recovery_sources_prepared_at(
                &std::collections::BTreeSet::from(["legacy".into()]),
                Some(at)
            )
            .is_err());
        let error = coverage
            .recovery_sources_prepared_at(
                &std::collections::BTreeSet::from(["missing".into()]),
                Some(at),
            )
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::IncompleteRecoverySource(id)) if id == "missing"
        ));
        runtime.inner.runtime_db.transaction(|tx| {
            tx.execute_batch("ALTER TABLE tasks RENAME TO tasks_capture_forbidden")?;
            Ok(())
        })?;
        for later in [
            at + chrono::Duration::milliseconds(1),
            at + chrono::Duration::days(1),
        ] {
            mutation
                .prepare_dispatch_tasks(&request, Some(later))
                .await?;
            let reused = mutation.capture_published_root()?;
            assert_eq!(prepared.token, reused.token);
            assert_eq!(
                reused.tasks.as_ref().unwrap().coverage().recovery_at,
                Some(at)
            );
            assert!(Arc::ptr_eq(
                prepared.tasks.as_ref().unwrap(),
                reused.tasks.as_ref().unwrap()
            ));
            mutation.capture_tasks(&request)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn named_task_prepare_publishes_once_and_pure_capture_never_queries_tasks() -> Result<()>
    {
        let home = tempdir()?;
        let runtime = RuntimeHandle::new(
            "task-owner",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "task-owner".into(),
            context_config(),
        )?;
        // A state-only acknowledgement explicitly leaves this domain unprepared.
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let mut reservation = runtime.inner.mutation_owner.reserve().await?;
        reservation.begin(crate::ids::audit_event_id())?;
        reservation.committed_prepared_with_domains(
            root.state.clone(),
            root.episodes.clone(),
            root.input_evidence.clone(),
            root.work.clone(),
            None,
        )?;
        reservation.publish(root.state.clone())?;
        drop(reservation);
        let mut mutation = runtime.begin_mutation().await?;
        assert!(mutation
            .capture_tasks(&TaskCaptureRequest::current())
            .is_err());
        mutation
            .prepare_owner_task_request(&TaskCaptureRequest::current())
            .await?;
        let prepared = mutation.capture_published_root()?;
        assert!(prepared.tasks.is_some());
        mutation
            .prepare_owner_task_request(&TaskCaptureRequest::current())
            .await?;
        assert_eq!(prepared.token, mutation.capture_published_root()?.token);
        runtime.inner.runtime_db.transaction(|tx| {
            tx.execute_batch("ALTER TABLE tasks RENAME TO tasks_capture_forbidden")?;
            Ok(())
        })?;
        let captured = mutation.capture_tasks(&TaskCaptureRequest::current())?;
        assert!(captured.records.is_empty());
        assert!(captured
            .lookup(&TaskKey::new(TaskFamily::Task, "never-observed"))
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_task_preparation_omits_unrequested_history_and_preserves_published_window(
    ) -> Result<()> {
        use crate::types::{TaskKind, TaskRecord, TaskStatus};

        let home = tempdir()?;
        let runtime = RuntimeHandle::new(
            "dispatch-task-owner",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "dispatch-task-owner".into(),
            context_config(),
        )?;
        let at = chrono::Utc::now();
        let active = TaskRecord {
            id: "dispatch-active".into(),
            agent_id: "dispatch-task-owner".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Queued,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        };
        let terminal = TaskRecord {
            id: "dispatch-terminal".into(),
            status: TaskStatus::Cancelled,
            ..active.clone()
        };
        runtime
            .persist_task_transition(&active, "task_queued")
            .await?;
        runtime
            .persist_task_transition(&terminal, "task_cancelled")
            .await?;

        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let mut reservation = runtime.inner.mutation_owner.reserve().await?;
        reservation.begin(crate::ids::audit_event_id())?;
        reservation.committed_prepared_with_domains(
            root.state.clone(),
            root.episodes.clone(),
            root.input_evidence.clone(),
            root.work.clone(),
            None,
        )?;
        reservation.publish(root.state.clone())?;
        drop(reservation);

        let lease =
            runtime.register_task_query(super::super::super::task_queries::TaskQuerySpec {
                output_task_ids: &["dispatch-missing"],
                ..Default::default()
            })?;
        let missing = TaskKey::new(TaskFamily::Task, "dispatch-missing");
        let terminal_key = TaskKey::new(TaskFamily::Task, &terminal.id);
        let request = super::super::super::task_reads::active_task_request();
        let mut mutation = runtime.begin_mutation().await?;
        mutation.prepare_dispatch_tasks(&request, None).await?;
        let prepared = mutation.capture_published_root()?;
        let tasks = prepared.tasks.as_ref().unwrap();
        assert!(tasks.coverage().active_task_ids.contains(&active.id));
        assert!(tasks.coverage().unsettled_complete);
        assert!(tasks.coverage().unsettled_result_ids.is_empty());
        assert_eq!(tasks.coverage().terminal.limit, 0);
        assert!(!tasks.coverage().terminal.is_exhaustive);
        assert!(!tasks.coverage().observations.contains_key(&terminal_key));
        assert!(tasks
            .capture_query(lease.selection())?
            .lookup(&missing)?
            .is_none());
        mutation.prepare_dispatch_tasks(&request, None).await?;
        assert_eq!(prepared.token, mutation.capture_published_root()?.token);

        let unrelated =
            runtime.register_task_query(super::super::super::task_queries::TaskQuerySpec {
                reply_message_ids: &["unprepared-reply"],
                ..Default::default()
            })?;
        assert!(tasks.capture_query(unrelated.selection()).is_err());
        mutation.prepare_context_tasks(&request).await?;
        assert_eq!(prepared.token, mutation.capture_published_root()?.token);
        drop(unrelated);

        let history = TaskCaptureRequest::current();
        mutation.prepare_dispatch_tasks(&history, None).await?;
        let mut requested = request;
        requested
            .required_keys
            .insert(TaskKey::new(TaskFamily::Task, "new-exact-observation"));
        mutation.prepare_dispatch_tasks(&requested, None).await?;
        let published = mutation.capture_published_root()?;
        let tasks = published.tasks.as_ref().unwrap();
        assert_eq!(tasks.coverage().terminal.limit, history.terminal_limit);
        assert!(tasks.capture(&history)?.lookup(&terminal_key)?.is_some());
        assert!(tasks
            .capture_query(lease.selection())?
            .lookup(&missing)?
            .is_none());
        Ok(())
    }

    #[tokio::test]
    async fn live_query_request_drops_prior_coverage_role_at_the_next_actual_task_ack() -> Result<()>
    {
        let home = tempdir()?;
        let runtime = RuntimeHandle::new(
            "task-query-owner",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "task-query-owner".into(),
            context_config(),
        )?;
        let lease =
            runtime.register_task_query(super::super::super::task_queries::TaskQuerySpec {
                output_task_ids: &["original-exact-query"],
                ..Default::default()
            })?;
        let mut mutation = runtime.begin_mutation().await?;
        mutation.prepare_owner_task_query(&lease).await?;
        let key = TaskKey::new(TaskFamily::Task, "original-exact-query");
        assert!(mutation
            .tasks
            .as_ref()
            .unwrap()
            .coverage()
            .required_keys
            .contains(&key));
        let (request, material) = mutation.owner_task_load_request()?;
        assert!(request.capture.required_keys.contains(&key));
        drop((request, material, mutation, lease));
        let mutation = runtime.begin_mutation().await?;
        assert!(
            mutation
                .tasks
                .as_ref()
                .unwrap()
                .coverage()
                .required_keys
                .contains(&key),
            "old published observation remains until an actual writer replaces it"
        );
        let (request, material) = mutation.owner_task_load_request()?;
        assert!(
            !request.capture.required_keys.contains(&key),
            "an old coverage selector is not current query authority"
        );
        drop((request, material, mutation));
        let at = chrono::Utc::now();
        let actual = crate::types::TaskRecord {
            id: "actual-next-task".into(),
            agent_id: "task-query-owner".into(),
            kind: crate::types::TaskKind::CommandTask,
            status: crate::types::TaskStatus::Queued,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        };
        runtime
            .persist_task_transition(&actual, "task_queued")
            .await?;
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert!(!root
            .tasks
            .as_ref()
            .unwrap()
            .coverage()
            .required_keys
            .contains(&key));
        assert!(runtime
            .capture_current_task_record("actual-next-task")?
            .record()
            .is_some());
        Ok(())
    }

    #[tokio::test]
    async fn task_query_lease_cannot_prepare_another_runtime_owner() -> Result<()> {
        let first = tempdir()?;
        let second = tempdir()?;
        let build = |home: &std::path::Path, id: &str| {
            RuntimeHandle::new(
                id,
                home.into(),
                None::<crate::types::WorkspaceEntry>,
                String::new(),
                Arc::new(StubProvider::new("unused")),
                id.into(),
                context_config(),
            )
        };
        let original = build(first.path(), "original-query-owner")?;
        let other = build(second.path(), "other-query-owner")?;
        let lease =
            original.register_task_query(super::super::super::task_queries::TaskQuerySpec {
                output_task_ids: &["exact-task"],
                ..Default::default()
            })?;
        let mut mutation = other.begin_mutation().await?;
        let before = mutation.capture_published_root()?.token;
        let error = mutation
            .prepare_owner_task_query(&lease)
            .await
            .err()
            .context("another owner accepted a foreign query lease")?;
        assert!(error.to_string().contains("another runtime scope or owner"));
        assert_eq!(mutation.capture_published_root()?.token, before);
        Ok(())
    }
}
