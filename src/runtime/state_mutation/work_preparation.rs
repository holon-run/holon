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
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let borrowed_scope = root
            .input_evidence
            .as_ref()
            .map(|inputs| inputs.scope())
            .or_else(|| root.work.as_ref().map(|work| work.scope()));
        let mut scope_cost = (0usize, 0usize);
        let _scope_material = if borrowed_scope.is_none() {
            let reservation = &self.reservations[self.primary_index];
            let bytes = usize::try_from(super::super::input_evidence_budget::serialized_bytes(
                &(reservation.agent_id(), reservation.identity_incarnation()),
            )?)?
            .checked_add(std::mem::size_of::<
                super::super::resident_input_evidence::InputEvidenceScope,
            >())
            .and_then(|n| n.checked_mul(8))
            .context("Work owner scope metadata overflow")?;
            anyhow::ensure!(
                bytes <= byte_limit.min(64 * 1024 * 1024) && 8 <= record_limit.min(65_536),
                crate::runtime_db::work_snapshot::WorkSnapshotError::ResourceExhausted(
                    "Work owner scope"
                )
            );
            scope_cost = (8, bytes);
            Some(
                self.runtime
                    .inner
                    .input_evidence_budget
                    .register_host_owned_bytes(
                        super::super::input_evidence_budget::InputMemoryCategory::Candidate,
                        super::super::input_evidence_budget::OwnedBytesEstimate {
                            payload_bytes: 0,
                            metadata_estimate_bytes: u64::try_from(bytes)?,
                        },
                    )?,
            )
        } else {
            None
        };
        let owned_scope = borrowed_scope
            .is_none()
            .then(|| self.input_evidence_scope());
        let scope = borrowed_scope
            .or(owned_scope.as_ref())
            .context("original Work owner scope missing")?;
        let _selector_material = work_snapshot::reserve_work_selector_material(
            scope,
            request,
            record_limit.min(65_536).saturating_sub(scope_cost.0),
            byte_limit
                .min(64 * 1024 * 1024)
                .saturating_sub(scope_cost.1),
            &self.runtime.inner.input_evidence_budget,
        )?;
        if let Some(work) = &root.work {
            anyhow::ensure!(work.scope() == scope, "published Work scope differs");
            match work.capture(request) {
                Ok(captured) => {
                    let ready = captured.records.iter().all(|record| match &record.record {
                        crate::runtime::resident_work::CanonicalWorkRecord::Work(work) => {
                            captured.legacy_delivery_heads.contains_key(&work.id)
                        }
                        _ => true,
                    });
                    let needs_inputs = captured.legacy_delivery_heads.values().any(|heads| {
                        heads.latest_any.body_id().is_some()
                            || heads.latest_nonempty.body_id().is_some()
                    }) || captured.latest_results.values().any(Option::is_some);
                    let inputs_ready = !needs_inputs
                        || root.input_evidence.as_ref().is_some_and(|inputs| {
                            inputs.covers_work_input_sources(work.coverage(), &Default::default())
                        });
                    if ready && inputs_ready {
                        return Ok(());
                    }
                }
                Err(error) if error.downcast_ref::<WorkReadError>().is_some() => {}
                Err(error) => return Err(error),
            }
        }
        let selector_cost = work_snapshot::work_selector_material_estimate(scope, request)?;
        let owner_prior = (
            scope_cost
                .1
                .checked_add(selector_cost.0)
                .context("Work owner prior bytes overflow")?,
            scope_cost
                .0
                .checked_add(selector_cost.1)
                .context("Work owner prior records overflow")?,
        );
        let load_request = WorkDomainLoadRequest {
            capture: request.clone(),
            record_limit,
            byte_limit,
        };
        let (work, inputs, _input_material) = self.runtime.inner.runtime_db.transaction(|tx| {
            let (prepared, inputs) = work_snapshot::prepare_work_with_owning_inputs_tx(
                tx,
                scope,
                &load_request,
                true,
                &self.runtime.inner.input_evidence_budget,
                root.input_evidence.as_deref(),
                owner_prior,
            )?;
            anyhow::ensure!(
                prepared.facts.state.as_ref() == Some(root.state.as_ref())
                    && prepared.facts.control_revision.is_some(),
                "Work preparation canonical State differs from the published root"
            );
            let identity = prepared
                .facts
                .identity
                .as_ref()
                .context("Work preparation identity missing")?;
            anyhow::ensure!(
                identity.agent_id == scope.agent_id
                    && identity.incarnation == scope.identity_incarnation,
                "Work preparation incarnation differs"
            );
            let material = prepared.material_views;
            let inputs = inputs
                .map(|inputs| inputs.with_budget(self.runtime.inner.input_evidence_budget.clone()))
                .transpose()?;
            // Budget rejection rolls sidecars back together. Keeping an old
            // Ready root must not leave a new unpublishable Work revision.
            Ok((
                prepared
                    .resident
                    .with_budget(self.runtime.inner.input_evidence_budget.clone())?,
                inputs,
                material,
            ))
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
        if let Some(inputs) = inputs {
            self.input_evidence = Some(Arc::new(inputs));
        }
        let facts = Arc::new(self.facts(Default::default())?);
        let reservation = &mut self.reservations[self.primary_index];
        reservation.begin(crate::ids::audit_event_id())?;
        MutationReservation::committed_all(
            std::slice::from_mut(reservation),
            vec![KnownCommit::State {
                current_reads: facts.current_reads.clone(),
                state: facts.state.clone(),
                episodes: facts.episodes.clone(),
                input_evidence: facts.input_evidence.clone(),
                work: facts.work.clone(),
                tasks: facts.tasks.clone(),
                scheduling: facts.scheduling.clone(),
                runtime: Some(facts.clone()),
            }],
        )?;
        // This original named preparation may change Work coverage/revision
        // without changing any file. Reuse only the strict same-source header
        // rebind; changed plan membership/flags remain Dirty for real refresh.
        self.runtime.resources_changed_after_commit(
            &root.state,
            root.work.as_deref(),
            &facts.state,
            facts.work.as_deref(),
        );
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
        self.base_work = self.work.clone();
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
                None,
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
                    effects: Box::new(crate::runtime_db::transitions::PostCommitEffects {
                        work_delta: Some(facts.delta),
                        ..Default::default()
                    }),
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
    async fn acknowledged_business_noop_installs_original_prepared_work_and_named_read_is_idempotent(
    ) -> Result<()> {
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
        assert!(mutation
            .capture_work(&WorkCaptureRequest::current())
            .is_err());
        let (accepted_revision, accepted_json): (u64, String) = runtime.inner.runtime_db.connection()?.query_row(
            "SELECT revision,coverage_json FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            rusqlite::params![scope.agent_id,scope.identity_incarnation], |row|Ok((row.get(0)?,row.get(1)?)),
        )?;
        let accepted: crate::runtime::resident_work::WorkCoverage =
            serde_json::from_str(&accepted_json)?;
        assert!(accepted.legacy_delivery_heads.is_empty());
        let state = mutation.state.clone();
        let nonce = crate::ids::audit_event_id();
        let prepared = PreparedInputTransition::focus(
            scope,
            nonce.clone(),
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
        // The business Focus is a no-op. Its original W1 preparation still
        // reads a complete qualified Work snapshot before the actual ACK; that
        // prepared domain completes previously unobserved Delivery head metadata.
        // That source observation can advance the domain revision even though
        // the business Work record and applied flag do not change.
        let commit = mutation.commit_prepared_input_transition(prepared).await?;
        assert!(!commit.applied);
        assert!(commit.effects.work_delta.is_none());
        let published = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("acknowledged no-op must leave the original root readable")?;
        let installed = published
            .work
            .as_ref()
            .context("original W1 preparation omitted its complete Work")?
            .clone();
        let acknowledged_scope = mutation.input_evidence_scope();
        let (acknowledged_revision, acknowledged_json): (u64, String) = runtime.inner.runtime_db.connection()?.query_row(
            "SELECT revision,coverage_json FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            rusqlite::params![acknowledged_scope.agent_id,acknowledged_scope.identity_incarnation],
            |row|Ok((row.get(0)?,row.get(1)?)),
        )?;
        let acknowledged: crate::runtime::resident_work::WorkCoverage =
            serde_json::from_str(&acknowledged_json)?;
        assert_eq!(installed.coverage(), &acknowledged);
        assert_eq!(
            acknowledged_revision,
            accepted_revision.checked_add(1).unwrap()
        );
        let heads = acknowledged
            .legacy_delivery_heads
            .get(&work.id)
            .context("actual W1 omitted complete Delivery head absence")?;
        assert!(matches!(
            heads.latest_any,
            crate::runtime::resident_work::WorkDeliveryHead::Absent
        ));
        assert!(matches!(
            heads.latest_nonempty,
            crate::runtime::resident_work::WorkDeliveryHead::Absent
        ));
        let receipt_json: String = runtime.inner.runtime_db.connection()?.query_row(
            "SELECT outcome_json FROM input_mutation_receipts WHERE command_nonce=?1",
            [&nonce],
            |row| row.get(0),
        )?;
        let outcome: crate::runtime_db::input_mutation_receipts::InputMutationOutcome =
            serde_json::from_str(&receipt_json)?;
        assert!(!outcome.applied);
        assert_eq!(outcome.scopes.len(), 1);
        assert_eq!(outcome.scopes[0].scope, acknowledged_scope);
        // These receipt keys describe actual business writes, not the qualified
        // Work domain installed from this acknowledgement's readonly preparation.
        assert!(outcome.scopes[0].install_keys.work_items.is_empty());
        let selected = installed.capture(&WorkCaptureRequest::current())?;
        assert_eq!(selected.work_item(&work.id)?, Some(&work));
        drop(selected);
        assert_eq!(
            runtime.inner.runtime_db.connection()?.query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
                [&nonce],
                |row| row.get::<_, usize>(0),
            )?,
            1
        );
        assert_eq!(
            runtime.inner.runtime_db.work_items().latest(&work.id)?,
            Some(work.clone())
        );
        drop(published);
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
        let after = mutation.capture_published_root()?;
        assert!(Arc::ptr_eq(after.work.as_ref().unwrap(), &installed));
        assert_eq!(
            after.work.as_ref().unwrap().coverage().revision,
            acknowledged_revision
        );
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

    #[tokio::test]
    async fn work_selector_budget_precedes_named_capture_clone_and_body_sql() -> Result<()> {
        let (_home, runtime) = fixture().await;
        let limits = runtime.inner.input_evidence_budget.limits();
        let byte_limit = usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("fixture Work limit overflow")?,
        )?
        .min(64 * 1024 * 1024);
        let mut request = WorkCaptureRequest::current();
        request
            .required_task_ids
            .insert("x".repeat(byte_limit / 8 + 1));
        let mut owner = runtime.begin_mutation().await?;
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE agent_states RENAME TO selector_state_forbidden;
                ALTER TABLE agent_identities RENAME TO selector_identity_forbidden;
                ALTER TABLE work_items RENAME TO selector_work_forbidden",
            )?;
            Ok(())
        })?;
        let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let error = owner
            .prepare_owner_work_request(&request)
            .await
            .err()
            .context("oversized borrowed Work selector was accepted")?;
        assert!(
            matches!(
                error.downcast_ref::<work_snapshot::WorkSnapshotError>(),
                Some(work_snapshot::WorkSnapshotError::ResourceExhausted(
                    "Work selectors"
                ))
            ),
            "{error:#}"
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            before
        );
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE selector_work_forbidden RENAME TO work_items;
                ALTER TABLE selector_identity_forbidden RENAME TO agent_identities;
                ALTER TABLE selector_state_forbidden RENAME TO agent_states",
            )?;
            Ok(())
        })?;
        assert!(runtime.inner.mutation_owner.capture()?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn actual_work_ack_from_partial_root_holds_material_through_known_publication_and_clones(
    ) -> Result<()> {
        use crate::runtime_db::transitions::{
            PreparedInputTransition, TransitionFaultPoint, WorkItemMutation,
            WorkItemTransitionCommand,
        };
        let (_home, runtime) = fixture().await;
        let mut mutation = runtime.begin_mutation().await?;
        let scope = mutation.input_evidence_scope();
        assert!(mutation.capture_published_root()?.work.is_none());
        let mut record = WorkItemRecord::new(
            &scope.agent_id,
            "guarded actual Known Work",
            WorkItemState::Open,
        );
        record.id = "original-writer-known-material".into();
        let nonce = crate::ids::audit_event_id();
        let prepared = PreparedInputTransition::work(
            scope.clone(),
            nonce.clone(),
            runtime.now(),
            WorkItemTransitionCommand {
                agent_id: scope.agent_id.clone(),
                mutation: WorkItemMutation::Insert {
                    record: record.clone(),
                },
                agent_state: None,
                brief_evidence: Vec::new(),
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: false,
                fault: Some(TransitionFaultPoint::BeforeCacheUpdate),
            },
            Default::default(),
            false,
        )?;
        let error = mutation
            .commit_prepared_input_transition(prepared)
            .await
            .err()
            .context("actual ACK must reach original publication fault")?;
        assert!(
            error
                .to_string()
                .contains("committed but publication requires reconciliation"),
            "{error:#}"
        );
        assert!(runtime.inner.mutation_owner.capture().is_err());
        let saved = mutation.reservations[mutation.primary_index]
            .recovery()?
            .context("original Known missing")?
            .committed_runtime
            .context("original Known omitted exact runtime facts")?;
        assert!(
            saved.effects.work_delta.is_none(),
            "authoritative Work delta must be consumed by the original domain"
        );
        let material = saved
            .effects
            .work_material_budget_views
            .as_ref()
            .context("original Known omitted actual Work readback material")?
            .clone();
        let weak = Arc::downgrade(&material[0]);
        let clone = material.clone();
        let body = runtime
            .inner
            .runtime_db
            .work_items()
            .latest(&record.id)?
            .context("original SQL ACK missing")?;
        assert_eq!(body, record);
        let receipts: i64 = runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [&nonce],
            |r| r.get(0),
        )?;
        assert_eq!(receipts, 1);
        drop(saved);
        runtime
            .reconcile_reserved_mutations(&mut mutation.reservations)
            .await?;
        let published = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("original publication missing")?;
        assert!(published
            .work
            .as_ref()
            .context("complete Work ACK missing")?
            .capture(&WorkCaptureRequest::current())?
            .live_work_ids
            .contains(&record.id));
        drop(published);
        assert!(weak.upgrade().is_some());
        let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        drop(material);
        assert!(weak.upgrade().is_some());
        drop(clone);
        assert!(
            weak.upgrade().is_none(),
            "Known publication must release original facts; only original material clones retain readback guards"
        );
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes < held);
        let after: i64 = runtime.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [&nonce],
            |r| r.get(0),
        )?;
        assert_eq!(after, 1, "publication must not rerun original SQL");
        assert_eq!(
            runtime.inner.runtime_db.work_items().latest(&record.id)?,
            Some(record)
        );
        Ok(())
    }
    #[tokio::test]
    async fn sparse_original_completion_known_ack_retains_material_without_work_domain(
    ) -> Result<()> {
        use crate::runtime_db::{
            prepared_input_evidence::{PreparedEvidenceValue, PreparedInputEvidence},
            transitions::{InputMutationExecution, PostCommitEffects, TransitionFaultPoint},
        };
        let (_home, runtime) = fixture().await;
        let mut owner = runtime.begin_mutation().await?;
        let scope = owner.input_evidence_scope();
        assert!(owner.capture_published_root()?.work.is_none());
        runtime.runtime_db().transaction(|tx| {
            tx.execute(
                "DELETE FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
                rusqlite::params![scope.agent_id, scope.identity_incarnation],
            )?;
            Ok(())
        })?;
        let record = crate::types::BriefRecord::new(
            &scope.agent_id,
            crate::types::BriefKind::Ack,
            "original sparse evidence",
            None,
            None,
        );
        let event = crate::types::brief_created_event_for(&record)?;
        let nonce = event.id.clone();
        let prepared = PreparedInputEvidence::brief_created(
            scope.clone(),
            nonce.clone(),
            record.created_at,
            record,
            event,
            Vec::new(),
        )?;
        let work_request = owner.owner_work_load_request()?;
        let (task_request, _selectors) = owner.owner_task_load_request()?;
        let ledger = runtime.inner.input_evidence_budget.clone();
        let db = runtime.runtime_db().clone();
        let error = owner
            .commit_transition(nonce.clone(), |_| {
                let InputMutationExecution::Committed {
                    value: ack,
                    receipt,
                } = db
                    .evidence()
                    .commit_prepared_input_evidence_with_task_budget(
                        &prepared,
                        &work_request,
                        &task_request,
                        &ledger,
                    )?
                else {
                    panic!("original sparse completion expected")
                };
                assert!(ack.work_delta.is_none());
                assert!(ack.work_material_budget_views.is_some());
                let PreparedEvidenceValue::BriefCreated(actual) = ack.record else {
                    panic!("Brief ACK expected")
                };
                Ok(TransitionCommit {
                    applied: receipt.outcome.applied,
                    effects: Box::new(PostCommitEffects {
                        agent_state: ack.agent_state,
                        input_evidence_delta: ack.input_delta,
                        work_delta: ack.work_delta,
                        work_material_budget_views: ack.work_material_budget_views,
                        task_delta: ack.task_delta,
                        task_delta_requires_prepared_domain: true,
                        task_material_budget_views: ack.task_material_budget_views,
                        briefs: vec![actual.brief],
                        fault: Some(TransitionFaultPoint::BeforeCacheUpdate),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            })
            .await
            .err()
            .context("actual ACK must reach original publication fault")?;
        assert!(format!("{error:#}").contains("publication"), "{error:#}");
        let KnownCommit::State {
            work,
            runtime: facts,
            ..
        } = owner.reservations[owner.primary_index]
            .recovery()?
            .unwrap()
            .known
            .unwrap()
        else {
            panic!("original Known State ACK expected")
        };
        assert!(work.is_none());
        let facts = facts.context("actual Known runtime facts missing")?;
        assert!(facts.effects.work_delta.is_none());
        let held = facts
            .effects
            .work_material_budget_views
            .as_ref()
            .unwrap()
            .clone();
        let weak = Arc::downgrade(&held[0]);
        assert!(weak.upgrade().is_some());
        runtime
            .reconcile_reserved_mutations(&mut owner.reservations)
            .await?;
        assert!(runtime
            .inner
            .mutation_owner
            .capture()?
            .unwrap()
            .work
            .is_none());
        assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM work_domain_current WHERE agent_id=?1 AND identity_incarnation=?2", rusqlite::params![scope.agent_id, scope.identity_incarnation], |row| row.get::<_, usize>(0))?, 0);
        assert_eq!(
            db.connection()?.query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
                [&nonce],
                |row| row.get::<_, usize>(0)
            )?,
            1
        );
        drop(facts);
        drop(owner);
        assert!(weak.upgrade().is_some());
        let observed = ledger.report()?.observed_bytes;
        drop(held);
        assert!(weak.upgrade().is_none());
        assert!(ledger.report()?.observed_bytes < observed);
        Ok(())
    }
}
