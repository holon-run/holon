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
        let (_, records, bytes) = self.context_input_evidence_policy()?;
        let estimate = super::super::input_evidence_budget::serialized_bytes(&owner)?
            .checked_add(128)
            .and_then(|n| n.checked_mul(8))
            .context("reply Work owner bytes overflow")?;
        anyhow::ensure!(
            records >= 8 && estimate <= u64::try_from(bytes)?,
            TaskReadError::ResourceExhausted("reply Work owner selector")
        );
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
        anyhow::ensure!(
            metadata <= u64::try_from(byte_limit)?
                && count
                    .checked_mul(4)
                    .context("current Task selector count overflow")?
                    <= record_limit,
            task_snapshot::TaskSnapshotError::ResourceExhausted("current Task request selectors")
        );
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
        self.prepare_owner_task_domain_request(&load).await
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
                ..TaskCaptureRequest::current()
            },
            record_limit,
            byte_limit,
        };
        self.prepare_owner_task_domain_request(&load).await
    }

    #[inline(never)]
    pub(in crate::runtime) fn prepare_owner_task_domain_request<'a>(
        &'a mut self,
        load: &'a TaskDomainLoadRequest,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.prepare_owner_task_domain_request_inner(load))
    }

    async fn prepare_owner_task_domain_request_inner(
        &mut self,
        load: &TaskDomainLoadRequest,
    ) -> Result<()> {
        let root = self.capture_published_root()?;
        let scope = self.input_evidence_scope();
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        anyhow::ensure!(
            load.record_limit > 0
                && load.record_limit <= record_limit
                && load.byte_limit > 0
                && load.byte_limit <= byte_limit,
            "Task query preparation exceeds the actual owner budget"
        );
        if let Some(tasks) = &root.tasks {
            anyhow::ensure!(tasks.scope() == &scope, "published Task scope differs");
            match tasks
                .capture_query(&load.query)
                .and_then(|_| tasks.capture(&load.capture))
            {
                Ok(_) => return Ok(()),
                Err(error) if error.downcast_ref::<TaskReadError>().is_some() => {}
                Err(error) => return Err(error),
            }
        }
        let tasks = self.runtime.inner.runtime_db.transaction(|tx| {
            let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
            anyhow::ensure!(
                facts.state.as_ref() == Some(root.state.as_ref())
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
        self.base = self.candidate.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        resident_task::{TaskFamily, TaskKey},
        tests::support::{context_config, tempdir, StubProvider},
    };

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
