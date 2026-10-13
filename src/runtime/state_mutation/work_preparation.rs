//! Named owner preparation. A capture never invokes this storage boundary.

use super::*;
use crate::{
    runtime::resident_work::{WorkCaptureRequest, WorkReadError},
    runtime_db::{
        context_inputs::ContextDurableRead,
        work_snapshot::{self, WorkDomainLoadRequest},
    },
};

impl RuntimeMutation {
    pub(in crate::runtime) fn owner_work_load_request(&self) -> Result<WorkDomainLoadRequest> {
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        Ok(WorkDomainLoadRequest {
            capture: WorkCaptureRequest::current(),
            record_limit,
            byte_limit,
        })
    }

    pub(super) fn install_committed_work_delta(
        &mut self,
        delta: &crate::runtime::resident_work::CommittedWorkDelta,
        requires_prepared_domain: bool,
    ) -> Result<()> {
        anyhow::ensure!(
            delta.scope == self.input_evidence_scope(),
            "committed Work scope differs"
        );
        if requires_prepared_domain && self.work.is_none() {
            // SQL is known committed; only a named full preparation can
            // supply the missing bodies of this sparse acknowledgement.
            return Ok(());
        }
        let next = if let Some(work) = &self.work {
            work.with_committed(delta)?
        } else {
            let request = self.owner_work_load_request()?;
            crate::runtime::resident_work::ResidentWorkDomain::bootstrap(
                delta.scope.clone(),
                delta.coverage.clone(),
                delta.records.clone(),
                request.record_limit,
                request.byte_limit,
            )?
            .with_budget(self.runtime.inner.input_evidence_budget.clone())?
        };
        self.work = Some(Arc::new(next));
        Ok(())
    }

    #[inline(never)]
    pub(in crate::runtime) fn prepare_owner_work_request<'a>(
        &'a mut self,
        request: &'a WorkCaptureRequest,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.prepare_owner_work_request_inner(request))
    }

    async fn prepare_owner_work_request_inner(
        &mut self,
        request: &WorkCaptureRequest,
    ) -> Result<()> {
        let root = self.capture_published_root()?;
        let scope = self.input_evidence_scope();
        if let Some(work) = &root.work {
            anyhow::ensure!(work.scope() == &scope, "published Work scope differs");
            match work.capture(request) {
                Ok(_) => return Ok(()),
                Err(error) if error.downcast_ref::<WorkReadError>().is_some() => {}
                Err(error) => return Err(error),
            }
        }
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let load_request = WorkDomainLoadRequest {
            capture: request.clone(),
            record_limit,
            byte_limit,
        };
        let work = self.runtime.inner.runtime_db.transaction(|tx| {
            let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
            anyhow::ensure!(
                facts.state.as_ref() == Some(root.state.as_ref())
                    && facts.control_revision.is_some(),
                "Work preparation canonical State differs from the published root"
            );
            let identity = facts
                .identity
                .as_ref()
                .context("Work preparation identity missing")?;
            anyhow::ensure!(
                identity.agent_id == scope.agent_id
                    && identity.incarnation == scope.identity_incarnation,
                "Work preparation incarnation differs"
            );
            let prepared = if identity.status == crate::types::AgentRegistryStatus::Deleting {
                // This same held reservation can prepare already-admitted
                // cleanup facts; it grants no activation or fresh writer.
                work_snapshot::prepare_original_work_recovery_snapshot_tx(
                    tx,
                    &scope,
                    &load_request,
                )?
            } else {
                work_snapshot::prepare_work_domain_snapshot_tx(tx, &scope, &load_request)?
            }
            .require_current()?;
            anyhow::ensure!(
                prepared.facts.identity == facts.identity
                    && prepared.facts.state == facts.state
                    && prepared.facts.control_revision == facts.control_revision,
                "Work preparation domains differ from the same read view"
            );
            // Budget rejection rolls sidecars back together. Keeping an old
            // Ready root must not leave a new unpublishable Work revision.
            prepared
                .resident
                .with_budget(self.runtime.inner.input_evidence_budget.clone())
        })?;
        let work = Arc::new(work);
        let current = self.reservations[self.primary_index]
            .capture_ready_snapshot()?
            .context("Work preparation lost its published root")?;
        anyhow::ensure!(
            current.token == root.token
                && current.state == root.state
                && current.token.owner_epoch == self.reservations[self.primary_index].owner_epoch(),
            "Work preparation reservation/publication generation differs"
        );
        self.work = Some(work);
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
                runtime: Some(facts.clone()),
            }],
        )?;
        // Save known installation responsibility before the first await.
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
    use crate::{
        runtime::{
            resident_work::{WorkFamily, WorkKey},
            tests::support::{context_config, tempdir, StubProvider},
        },
        types::{WorkItemRecord, WorkItemState},
    };

    fn raw_fixture() -> (tempfile::TempDir, RuntimeHandle) {
        let home = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "owner-work-preparation",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "owner-work-preparation".into(),
            context_config(),
        )
        .unwrap();
        runtime
            .inner
            .runtime_db
            .transaction(|tx| work_snapshot::install_work_domain_test_schema(tx))
            .unwrap();
        (home, runtime)
    }

    async fn fixture() -> (tempfile::TempDir, RuntimeHandle) {
        let (home, runtime) = raw_fixture();
        // Exercise the explicit unprepared boundary of an unmigrated state-only
        // writer even after normal bootstrap has a complete Work domain.
        let root = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let mut reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
        reservation.begin(crate::ids::audit_event_id()).unwrap();
        reservation
            .committed_prepared_with_inputs(
                root.state.clone(),
                root.episodes.clone(),
                root.input_evidence.clone(),
            )
            .unwrap();
        reservation.publish(root.state.clone()).unwrap();
        drop(reservation);
        (home, runtime)
    }

    #[tokio::test]
    async fn named_work_preparation_preserves_other_domains_and_is_idempotent() -> Result<()> {
        let (_home, runtime) = fixture().await;
        let mut mutation = runtime.begin_mutation().await?;
        let before = mutation.capture_published_root()?;
        let scope = mutation.input_evidence_scope();
        assert!(before.work.is_none());
        runtime.inner.runtime_db.transaction(|tx| {
            let work = WorkItemRecord::new(
                &scope.agent_id,
                "original qualified Work",
                WorkItemState::Open,
            );
            let keys = std::collections::BTreeSet::from([WorkKey::new(WorkFamily::Work, &work.id)]);
            let proof = work_snapshot::prepare_work_write_tx(tx, &scope, &keys)?;
            crate::runtime_db::repositories::insert_new_work_item_tx(tx, &work)?;
            let state = ContextDurableRead::new(tx, &scope.agent_id)
                .scope_facts()?
                .state
                .unwrap();
            work_snapshot::register_work_writes_tx(
                tx,
                &proof,
                &keys,
                &Default::default(),
                &state,
                &WorkDomainLoadRequest {
                    capture: WorkCaptureRequest::current(),
                    record_limit: 1024,
                    byte_limit: 1024 * 1024,
                },
            )?;
            Ok(())
        })?;
        let request = WorkCaptureRequest::current();
        mutation.prepare_owner_work_request(&request).await?;
        let after = mutation.capture_published_root()?;
        assert!(after.token.generation > before.token.generation);
        assert_eq!(after.state, before.state);
        assert!(Arc::ptr_eq(
            after.input_evidence.as_ref().unwrap(),
            before.input_evidence.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            after.episodes.as_ref().unwrap(),
            before.episodes.as_ref().unwrap()
        ));
        assert_eq!(mutation.capture_work(&request)?.live_work_ids.len(), 1);
        mutation.prepare_owner_work_request(&request).await?;
        assert_eq!(mutation.capture_published_root()?.token, after.token);
        Ok(())
    }

    #[tokio::test]
    async fn named_work_preparation_rejects_a_private_state_candidate_before_installation(
    ) -> Result<()> {
        let (_home, runtime) = fixture().await;
        let mut mutation = runtime.begin_mutation().await?;
        let before = mutation.capture_published_root()?;
        mutation.state.last_wake_reason = Some("private candidate".into());
        assert!(mutation
            .prepare_owner_work_request(&WorkCaptureRequest::current())
            .await
            .is_err());
        let current = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(current.token, before.token);
        assert!(current.work.is_none());
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .agent_states()
                .latest(&before.state.id)?
                .as_ref(),
            Some(before.state.as_ref())
        );
        Ok(())
    }

    #[tokio::test]
    async fn named_work_budget_rejection_rolls_back_sidecars_before_retry() -> Result<()> {
        let (_home, runtime) = fixture().await;
        let mut mutation = runtime.begin_mutation().await?;
        let root = mutation.capture_published_root()?;
        let scope = mutation.input_evidence_scope();
        runtime.inner.runtime_db.transaction(|tx| {
            let work =
                WorkItemRecord::new(&scope.agent_id, "bounded history", WorkItemState::Completed);
            let keys = std::collections::BTreeSet::from([WorkKey::new(WorkFamily::Work, &work.id)]);
            let proof = work_snapshot::prepare_work_write_tx(tx, &scope, &keys)?;
            crate::runtime_db::repositories::insert_new_work_item_tx(tx, &work)?;
            let state = ContextDurableRead::new(tx, &scope.agent_id)
                .scope_facts()?
                .state
                .unwrap();
            work_snapshot::register_work_writes_tx(
                tx,
                &proof,
                &keys,
                &Default::default(),
                &state,
                &WorkDomainLoadRequest {
                    capture: WorkCaptureRequest::current(),
                    record_limit: 1024,
                    byte_limit: 1024 * 1024,
                },
            )?;
            Ok(())
        })?;
        let coverage = || -> Result<String> {
            Ok(runtime.inner.runtime_db.connection()?.query_row("SELECT coverage_json FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2", rusqlite::params![scope.agent_id,scope.identity_incarnation], |row| row.get(0))?)
        };
        let before = coverage()?;
        let pressure = runtime.inner.input_evidence_budget.register_owned_bytes(
            &scope,
            super::super::super::input_evidence_budget::InputMemoryCategory::History,
            super::super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: runtime
                    .inner
                    .input_evidence_budget
                    .limits()
                    .history_per_agent_bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let mut request = WorkCaptureRequest::current();
        request.nonopen_limit = 1;
        assert!(mutation.prepare_owner_work_request(&request).await.is_err());
        assert_eq!(coverage()?, before);
        assert_eq!(
            runtime.inner.mutation_owner.capture()?.unwrap().token,
            root.token
        );
        assert!(runtime
            .inner
            .mutation_owner
            .capture()?
            .unwrap()
            .work
            .is_none());
        drop(pressure);
        mutation.prepare_owner_work_request(&request).await?;
        assert_eq!(mutation.capture_work(&request)?.nonopen.keys.len(), 1);
        Ok(())
    }

    fn original_work_commit(
        db: &crate::runtime_db::RuntimeDb,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        request: &WorkDomainLoadRequest,
        invalidate_delta: bool,
    ) -> Result<(WorkItemRecord, TransitionCommit)> {
        db.transaction(|tx| {
            let work =
                WorkItemRecord::new(&scope.agent_id, "actual original Work", WorkItemState::Open);
            let keys = std::collections::BTreeSet::from([WorkKey::new(WorkFamily::Work, &work.id)]);
            let proof = work_snapshot::prepare_work_write_tx(tx, scope, &keys)?;
            crate::runtime_db::repositories::insert_new_work_item_tx(tx, &work)?;
            let state = ContextDurableRead::new(tx, &scope.agent_id)
                .scope_facts()?
                .state
                .unwrap();
            let mut facts = work_snapshot::register_work_writes_tx(
                tx,
                &proof,
                &keys,
                &Default::default(),
                &state,
                request,
            )?;
            if invalidate_delta {
                facts.delta.scope.identity_incarnation += 1;
            }
            Ok((
                work,
                TransitionCommit {
                    applied: true,
                    effects: crate::runtime_db::transitions::PostCommitEffects {
                        work_delta: Some(facts.delta),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ))
        })
    }

    #[tokio::test]
    async fn original_work_ack_installs_one_root_and_strips_returned_authoritative_payloads(
    ) -> Result<()> {
        let (_home, runtime) = fixture().await;
        let mut mutation = runtime.begin_mutation().await?;
        let before = mutation.capture_published_root()?;
        let request = mutation.owner_work_load_request()?;
        let db = runtime.inner.runtime_db.clone();
        let mut written = None;
        let committed = mutation
            .commit_transition(crate::ids::audit_event_id(), |scope| {
                let (work, committed) = original_work_commit(&db, scope, &request, false)?;
                written = Some(work);
                Ok(committed)
            })
            .await?;
        let written = written.unwrap();
        assert!(committed.effects.work_delta.is_none());
        assert!(committed.effects.work_input_preparation.is_none());
        let after = mutation.capture_published_root()?;
        assert!(after.token.generation > before.token.generation);
        assert_eq!(after.state, before.state);
        let captured = after
            .work
            .as_ref()
            .unwrap()
            .capture(&WorkCaptureRequest::current())?;
        let record = captured
            .lookup(&WorkKey::new(WorkFamily::Work, &written.id))?
            .unwrap();
        assert_eq!(
            record.record,
            crate::runtime::resident_work::CanonicalWorkRecord::Work(written)
        );
        assert!(before.work.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn standalone_result_ack_preserves_unprepared_work_until_named_recovery() -> Result<()> {
        use crate::runtime_db::prepared_input_evidence::PreparedInputEvidence;
        use crate::types::{BriefKind, BriefRecord};
        let (_home, runtime) = fixture().await;
        let mut mutation = runtime.begin_mutation().await?;
        let scope = mutation.input_evidence_scope();
        let (work, _) = original_work_commit(
            &runtime.inner.runtime_db,
            &scope,
            &mutation.owner_work_load_request()?,
            false,
        )?;
        assert!(mutation.work.is_none());
        let mut result = BriefRecord::new(
            &scope.agent_id,
            BriefKind::Result,
            "actual result",
            None,
            None,
        );
        result.work_item_id = Some(work.id.clone());
        let event = crate::types::brief_created_event_for(&result)?;
        let prepared = || {
            PreparedInputEvidence::brief_created(
                scope.clone(),
                event.id.clone(),
                result.created_at,
                result.clone(),
                event.clone(),
                Vec::new(),
            )
        };
        let original = mutation.commit_prepared_evidence(prepared()?, None).await?;
        let acknowledged = mutation.capture_published_root()?;
        assert!(
            acknowledged.work.is_none(),
            "sparse Result ACK cannot supply missing Work bodies"
        );
        assert_eq!(
            runtime.inner.runtime_db.work_items().latest(&work.id)?,
            Some(work.clone())
        );
        let replay = mutation.commit_prepared_evidence(prepared()?, None).await?;
        assert_eq!(replay, original);
        let current = mutation.capture_published_root()?;
        let captured = current
            .work
            .as_ref()
            .context("fresh original recovery Work missing")?
            .capture(&WorkCaptureRequest::current())?;
        assert!(captured.live_work_ids.contains(&work.id));
        assert_eq!(
            captured.latest_results[&work.id].as_ref().unwrap().brief_id,
            result.id
        );
        assert!(acknowledged.work.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn work_projection_failure_preserves_known_sql_ack_and_gates_publication() -> Result<()> {
        let (_home, runtime) = fixture().await;
        let mut mutation = runtime.begin_mutation().await?;
        let request = mutation.owner_work_load_request()?;
        let db = runtime.inner.runtime_db.clone();
        let mut written = None;
        let result = mutation
            .commit_transition(crate::ids::audit_event_id(), |scope| {
                let (work, committed) = original_work_commit(&db, scope, &request, true)?;
                written = Some(work);
                Ok(committed)
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            runtime.inner.mutation_owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ));
        let known = mutation.reservations[mutation.primary_index]
            .recovery()?
            .unwrap()
            .known
            .unwrap();
        let KnownCommit::State {
            work,
            runtime: facts,
            ..
        } = known
        else {
            panic!("known SQL ACK missing");
        };
        assert!(work.is_none());
        assert!(facts.unwrap().effects.work_delta.is_none());
        let written = written.unwrap();
        assert_eq!(db.work_items().latest(&written.id)?, Some(written));
        Ok(())
    }

    #[tokio::test]
    async fn acknowledged_noop_with_unprepared_work_requires_named_installation() -> Result<()> {
        use crate::runtime_db::transitions::{
            AgentStateMutation, PreparedInputTransition, WorkItemFocusTransitionCommand,
        };
        let (_home, runtime) = fixture().await;
        let mut mutation = runtime.begin_mutation().await?;
        assert!(mutation.work.is_none());
        let scope = mutation.input_evidence_scope();
        let (work, _) = original_work_commit(
            &runtime.inner.runtime_db,
            &scope,
            &mutation.owner_work_load_request()?,
            false,
        )?;
        let state = mutation.state.clone();
        let prepared = PreparedInputTransition::focus(
            scope,
            crate::ids::audit_event_id(),
            Utc::now(),
            WorkItemFocusTransitionCommand {
                agent_id: state.id.clone(),
                work_items: Vec::new(),
                wait_conditions: Vec::new(),
                active_continuation_expectations: None,
                continuation_expectations: Vec::new(),
                continuations: Vec::new(),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(state.clone())),
                    record: Box::new(state),
                },
                brief_evidence: Vec::new(),
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: false,
                fault: None,
            },
            Default::default(),
            false,
            None,
        )?;
        assert!(mutation
            .commit_prepared_input_transition(prepared)
            .await
            .is_err());
        let known = mutation.reservations[mutation.primary_index]
            .recovery()?
            .unwrap()
            .known
            .unwrap();
        assert!(matches!(known, KnownCommit::State { work: None, .. }));
        assert!(matches!(
            runtime.inner.mutation_owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ));
        runtime
            .reconcile_reserved_mutations(&mut mutation.reservations)
            .await?;
        mutation
            .prepare_owner_work_request(&WorkCaptureRequest::current())
            .await?;
        assert!(mutation
            .capture_work(&WorkCaptureRequest::current())?
            .live_work_ids
            .contains(&work.id));
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_publishes_current_work_with_state_and_retains_actual_writer_qualification(
    ) -> Result<()> {
        let (home, runtime) = raw_fixture();
        let initial = runtime.inner.mutation_owner.capture()?.unwrap();
        assert!(initial
            .work
            .as_ref()
            .context("bootstrap Work missing")?
            .capture(&WorkCaptureRequest::current())?
            .live_work_ids
            .is_empty());
        let original = runtime
            .create_work_item("survives restart".into(), None, None, Vec::new())
            .await?;
        let retained = runtime.inner.mutation_owner.capture()?.unwrap();
        let restarted = RuntimeHandle::new(
            "owner-work-preparation",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "owner-work-preparation".into(),
            context_config(),
        )?;
        let current = restarted.inner.mutation_owner.capture()?.unwrap();
        let work = current.work.as_ref().context("restarted Work missing")?;
        assert_eq!(
            work.scope().identity_incarnation,
            current.token.identity_incarnation
        );
        assert_eq!(work.scope().agent_id, current.state.id);
        let captured = work.capture(&WorkCaptureRequest::current())?;
        let qualified = captured
            .lookup(&WorkKey::new(WorkFamily::Work, &original.id))?
            .unwrap();
        assert_eq!(
            qualified.record,
            crate::runtime::resident_work::CanonicalWorkRecord::Work(original)
        );
        assert_eq!(current.state.as_ref(), &restarted.agent_state().await?);
        assert!(retained
            .work
            .as_ref()
            .unwrap()
            .capture(&WorkCaptureRequest::current())?
            .live_work_ids
            .contains(&qualified.key.id));
        Ok(())
    }
}
