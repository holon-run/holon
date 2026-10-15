//! Preserve only the authentic initial Stop's reserved readonly member.
use super::*;
use crate::runtime_db::task_stop::{
    observe_task_stop_readonly_identity_tx, TaskStopChildTarget, TaskStopReadonlyIdentity,
};

#[derive(Debug)]
pub(in crate::runtime) struct InitialTaskStopReadonlyMember {
    observation: TaskStopReadonlyIdentity,
    known: KnownCommit,
}
impl InitialTaskStopReadonlyMember {
    pub(in crate::runtime) fn target(&self) -> &TaskStopChildTarget {
        self.observation.target()
    }
    pub(in crate::runtime) fn known(&self) -> KnownCommit {
        self.known.clone()
    }
    pub(in crate::runtime) fn runtime(&self) -> Option<&Arc<RuntimeMutationFacts>> {
        match &self.known {
            KnownCommit::State { runtime, .. } => runtime.as_ref(),
            _ => None,
        }
    }
}

impl RuntimeMutation {
    pub(super) fn validate_initial_readonly_stop(
        &self,
        prepared: &crate::runtime_db::transitions::PreparedInputTransition,
    ) -> Result<()> {
        for member in &self.task_stop_readonly {
            let original = prepared
                .initial_task_stop_intent()
                .context("readonly child requires the initial Stop intent")?;
            anyhow::ensure!(
                original.child.as_ref() == Some(member.target())
                    && original.task.scope == self.input_evidence_scope(),
                "readonly owner differs from the initial Stop child"
            );
        }
        Ok(())
    }
}

impl RuntimeHandle {
    /// This reserves a readonly group, but grants no child write/State authority.
    #[inline(never)]
    pub(in crate::runtime) fn begin_task_stop_with_readonly_child<'a>(
        &'a self,
        child: Option<&'a TaskStopChildTarget>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<RuntimeMutation>> + Send + 'a>>
    {
        Box::pin(async move {
            let Some(child) = child else {
                return self.begin_mutation().await;
            };
            anyhow::ensure!(
                child.parent_agent_id == self.inner.mutation_owner.agent_id()
                    && child.scope.agent_id != child.parent_agent_id
                    && child.scope.identity_incarnation > 0,
                "readonly Stop child is not a distinct original child"
            );
            let bytes = super::super::input_evidence_budget::serialized_bytes(child)?
                .checked_mul(8)
                .context("readonly target material overflow")?;
            let limits = self.inner.input_evidence_budget.limits();
            anyhow::ensure!(
                bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("readonly target limit overflow")?
                        .min(64 * 1024 * 1024)
                    && limits.record_limit >= 8,
                crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                    "Stop readonly target"
                )
            );
            let _target_material = self.inner.input_evidence_budget.register_owned_bytes(
                &child.scope,
                super::super::input_evidence_budget::InputMemoryCategory::Candidate,
                super::super::input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: bytes,
                },
            )?;
            let owner = match &self.inner.host_bridge {
                Some(bridge) => bridge
                    .mutation_owner_for(&child.scope.agent_id, child.scope.identity_incarnation)?,
                None => self
                    .inner
                    .standalone_related_mutation_owners
                    .current_owner(&child.scope.agent_id, child.scope.identity_incarnation)?,
            };
            self.begin_mutation_with_owners_inner(
                vec![self.inner.mutation_owner.clone(), owner],
                Some(child),
            )
            .await
        })
    }
    pub(super) async fn prepare_initial_stop_readonly_member(
        &self,
        reservation: &MutationReservation,
        target: &TaskStopChildTarget,
    ) -> Result<Arc<InitialTaskStopReadonlyMember>> {
        anyhow::ensure!(
            reservation.agent_id() == target.scope.agent_id
                && reservation.identity_incarnation() == target.scope.identity_incarnation,
            "readonly Stop reservation differs"
        );
        let limits = self.inner.input_evidence_budget.limits();
        let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: super::super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("readonly Stop limit overflow")?,
            )?,
        };
        let observation = self.inner.runtime_db.transaction(|tx| {
            observe_task_stop_readonly_identity_tx(
                tx,
                target,
                &request,
                &self.inner.input_evidence_budget,
            )
        })?;
        let root = reservation.capture_ready_snapshot()?;
        let known = if observation.state_present() == Some(true) {
            if let Some(root) = root {
                let runtime = if let Some(bridge) = &self.inner.host_bridge {
                    if let Some(runtime) = bridge.runtime_for_mutation(reservation).await? {
                        Some(Arc::new(
                            runtime
                                .prepare_external_input_facts(
                                    reservation,
                                    root.state.clone(),
                                    root.episodes.clone(),
                                    root.input_evidence.clone(),
                                )
                                .await?,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };
                KnownCommit::State {
                    current_reads: root.current_reads.clone(),
                    state: root.state.clone(),
                    episodes: root.episodes.clone(),
                    input_evidence: root.input_evidence.clone(),
                    work: root.work.clone(),
                    tasks: root.tasks.clone(),
                    scheduling: root.scheduling.clone(),
                    runtime,
                }
            } else {
                KnownCommit::Metadata(Arc::new(IdentityMutationFacts::observed(
                    observation.identity().clone(),
                    None,
                    observation.identity_material(),
                )))
            }
        } else {
            KnownCommit::Metadata(Arc::new(IdentityMutationFacts::observed(
                observation.identity().clone(),
                None,
                observation.identity_material(),
            )))
        };
        Ok(Arc::new(InitialTaskStopReadonlyMember {
            observation,
            known,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::StubProvider,
        runtime::{
            mutation_owner::MutationOwner,
            mutation_witness::MutationWitness,
            resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
            task_stop_intent::FrozenTaskStopIntent,
        },
        runtime_db::{
            input_mutation_receipts::OriginalInputMutationAttempt,
            task_stop::{load_task_stop_by_sequence_tx, TaskStopProgressAction},
            transitions::{PreparedInputTransition, PreparedTaskTransition, TaskTransitionCommand},
        },
        types::{
            AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
            TaskKind, TaskRecord, TaskStatus,
        },
    };

    struct Fixture {
        _home: tempfile::TempDir,
        parent: RuntimeHandle,
        child: RuntimeHandle,
        target: TaskStopChildTarget,
        child_owner: mutation_owner::MutationOwner,
        task: TaskRecord,
    }
    async fn fixture() -> Result<Fixture> {
        let home = tempfile::tempdir()?;
        let make = |id: &str| {
            RuntimeHandle::new(
                id,
                home.path().into(),
                None::<crate::types::WorkspaceEntry>,
                String::new(),
                Arc::new(StubProvider::new("unused")),
                "default".into(),
                crate::context::ContextConfig::default(),
            )
        };
        let parent = make("default")?;
        let child = make("task-stop-child")?;
        let at = parent.now();
        let task = TaskRecord {
            id: "task:readonly-stop".into(),
            agent_id: "default".into(),
            kind: TaskKind::SubagentTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({"child_agent_id":"task-stop-child"})),
            recovery: None,
        };
        parent
            .persist_task_transition(&task, "readonly_fixture")
            .await?;
        let identity = AgentIdentityRecord::new(
            "task-stop-child",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some("default".into()),
            Some(task.id.clone()),
        );
        parent
            .inner
            .runtime_db
            .agent_identities()
            .upsert(&identity)?;
        let target = TaskStopChildTarget {
            scope: super::super::super::resident_input_evidence::InputEvidenceScope {
                agent_id: identity.agent_id.clone(),
                identity_incarnation: identity.incarnation,
            },
            parent_agent_id: "default".into(),
            creation_task_id: task.id.clone(),
        };
        let child_owner = parent
            .inner
            .standalone_related_mutation_owners
            .current_owner(&target.scope.agent_id, target.scope.identity_incarnation)?;
        let root = child
            .inner
            .mutation_owner
            .capture()?
            .context("child original publication")?;
        let mut reservation = child_owner.try_reserve()?;
        reservation.begin("seed-child-publication".into())?;
        reservation.committed_prepared_with_domains(
            root.state.clone(),
            root.episodes.clone(),
            root.input_evidence.clone(),
            root.work.clone(),
            root.tasks.clone(),
        )?;
        reservation.publish(root.state.clone())?;
        drop(reservation);
        Ok(Fixture {
            _home: home,
            parent,
            child,
            target,
            child_owner,
            task,
        })
    }
    fn request() -> crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: TaskCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        }
    }
    async fn prepared(
        f: &Fixture,
        mutation: &mut RuntimeMutation,
        nonce: &str,
    ) -> Result<PreparedInputTransition> {
        let mut request = TaskCaptureRequest::current();
        request
            .required_keys
            .insert(TaskKey::new(TaskFamily::Task, &f.task.id));
        mutation.prepare_owner_task_request(&request).await?;
        let captured = mutation.capture_tasks(&request)?;
        let qualified = captured
            .lookup(&TaskKey::new(TaskFamily::Task, &f.task.id))?
            .context("original Task")?;
        let frozen = FrozenTaskStopIntent::prepare(
            qualified,
            None,
            f.parent.instance_owner_epoch(),
            Some(&f.target),
            f.parent.now(),
            &f.parent.inner.input_evidence_budget,
        )?;
        let command = TaskTransitionCommand {
            agent_id: "default".into(),
            task: frozen.task.clone(),
            agent_deletion: None,
            task_result_settlement: None,
            queue_entry: None,
            work_items: vec![],
            expected_wait_conditions: vec![],
            wait_conditions: vec![],
            agent_state: None,
            message_evidence: vec![],
            audit_events: vec![],
            index_changes: vec![],
            notify_scheduler: true,
            commit_on_idempotent: false,
            fault: None,
        };
        PreparedInputTransition::task(
            mutation.input_evidence_scope(),
            nonce.into(),
            frozen.task.updated_at,
            PreparedTaskTransition::new(command, Default::default(), false, None, None)?
                .with_stop_intent(frozen.intent.clone())?,
        )
    }
    fn assert_domains_same(
        a: &super::super::super::resident_state::AgentSnapshot,
        b: &super::super::super::resident_state::AgentSnapshot,
    ) {
        assert!(Arc::ptr_eq(&a.state, &b.state));
        assert!(Arc::ptr_eq(
            a.episodes.as_ref().unwrap(),
            b.episodes.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            a.input_evidence.as_ref().unwrap(),
            b.input_evidence.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            a.work.as_ref().unwrap(),
            b.work.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            a.tasks.as_ref().unwrap(),
            b.tasks.as_ref().unwrap()
        ));
    }

    #[tokio::test]
    async fn readonly_initial_stop_keeps_child_published_domains_without_reading_its_state_body(
    ) -> Result<()> {
        let f = fixture().await?;
        let old = f.child_owner.capture()?.unwrap();
        // Poisoned current body proves this adapter only observes scalar presence.
        f.parent.inner.runtime_db.connection()?.execute(
            "UPDATE agent_states SET payload_json='{bad' WHERE agent_id=?1",
            [&f.target.scope.agent_id],
        )?;
        let mut mutation = f
            .parent
            .begin_task_stop_with_readonly_child(Some(&f.target))
            .await?;
        let prepared = prepared(&f, &mut mutation, "readonly-normal").await?;
        mutation.commit_initial_task_stop(prepared).await?;
        drop(mutation);
        assert_domains_same(&old, &f.child_owner.capture()?.unwrap());
        assert_eq!(f.child.inner.agent.lock().await.state, *old.state);
        Ok(())
    }

    #[tokio::test]
    async fn readonly_initial_lost_ack_uses_exact_whole_receipt_and_preserves_original_child_root(
    ) -> Result<()> {
        let f = fixture().await?;
        let old = f.child_owner.capture()?.unwrap();
        let mut mutation = f
            .parent
            .begin_task_stop_with_readonly_child(Some(&f.target))
            .await?;
        let prepared = prepared(&f, &mut mutation, "readonly-lost-ack")
            .await?
            .with_scope_group(
                mutation
                    .reservations
                    .iter()
                    .map(
                        |r| super::super::super::resident_input_evidence::InputEvidenceScope {
                            agent_id: r.agent_id().into(),
                            identity_incarnation: r.identity_incarnation(),
                        },
                    )
                    .collect(),
            )?;
        let witness = MutationWitness::input_mutation(
            "readonly-lost-ack".into(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            vec![],
        )?
        .with_input_runtime(vec![Arc::new(mutation.facts(Default::default())?)])?
        .with_initial_task_stop_readonly(mutation.task_stop_readonly.clone())?;
        MutationReservation::begin_all_with_witness(
            &mut mutation.reservations,
            "readonly-lost-ack".into(),
            witness.clone(),
        )?;
        f.parent
            .inner
            .runtime_db
            .transitions_in_scope(&mutation.input_evidence_scope())
            .commit_prepared_transition(&prepared)?;
        witness.finish_original_attempt();
        drop(mutation);
        f.parent.inner.runtime_db.connection()?.execute(
            "UPDATE agent_states SET payload_json='{bad' WHERE agent_id=?1",
            [&f.target.scope.agent_id],
        )?;
        f.parent.reconcile_posture_observation().await?;
        assert_domains_same(&old, &f.child_owner.capture()?.unwrap());
        let receipt_count: i64 = f.parent.inner.runtime_db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='readonly-lost-ack'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(receipt_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn readonly_child_absent_or_retired_state_is_not_loaded_and_wrong_target_cannot_commit(
    ) -> Result<()> {
        for retired in [false, true] {
            let f = fixture().await?;
            if retired {
                let mut identity = f
                    .parent
                    .inner
                    .runtime_db
                    .agent_identities()
                    .latest(&f.target.scope.agent_id)?
                    .unwrap();
                identity.status = crate::types::AgentRegistryStatus::Deleted;
                f.parent
                    .inner
                    .runtime_db
                    .agent_identities()
                    .upsert(&identity)?;
                f.parent.inner.runtime_db.connection()?.execute(
                    "UPDATE agent_states SET payload_json='{bad' WHERE agent_id=?1",
                    [&f.target.scope.agent_id],
                )?;
            } else {
                f.parent.inner.runtime_db.connection()?.execute(
                    "DELETE FROM agent_states WHERE agent_id=?1",
                    [&f.target.scope.agent_id],
                )?;
            }
            let mut mutation = f
                .parent
                .begin_task_stop_with_readonly_child(Some(&f.target))
                .await?;
            let prepared = prepared(&f, &mut mutation, "readonly-no-state").await?;
            mutation.commit_initial_task_stop(prepared).await?;
            drop(mutation);
            assert!(f.child_owner.capture()?.is_none());
            let mut wrong = f.target.clone();
            wrong.creation_task_id = "another-task".into();
            assert!(f
                .parent
                .begin_task_stop_with_readonly_child(Some(&wrong))
                .await
                .is_err());
        }
        Ok(())
    }

    #[tokio::test]
    async fn sparse_phase_recovery_retains_accepted_accounting_and_does_not_hydrate_incomplete_state(
    ) -> Result<()> {
        let f = fixture().await?;
        // A real Command lease keeps Physical Pending; MissingProof from
        // NoHandle is terminal and cannot be rewritten to NeedsAttention.
        let mut command_task = f.task.clone();
        command_task.id = "task:sparse-command".into();
        command_task.kind = TaskKind::CommandTask;
        command_task.updated_at = f.parent.now();
        command_task.detail = Some(serde_json::json!({"cancel_requested":false}));
        f.parent
            .persist_task_transition(&command_task, "sparse_original_command")
            .await?;
        let (cancel, _cancel_rx) = tokio::sync::oneshot::channel();
        let (force, _force_rx) = tokio::sync::oneshot::channel();
        let cancel = crate::runtime::control_cancellation::SharedControlSignal::new(cancel);
        let force = crate::runtime::control_cancellation::SharedControlSignal::new(force);
        let lease = f.parent.inner.control_cancellations.command(
            &command_task,
            cancel.clone(),
            force.clone(),
        );
        let (input_tx, _input_rx) = tokio::sync::mpsc::channel(1);
        let handle = crate::runtime::command_task::ManagedTaskHandle::Command(
            crate::runtime::command_task::CommandTaskHandle {
                cancel_tx: Some(cancel),
                force_stop_tx: Some(force),
                input_tx,
                control_lease: Some(lease),
            },
        );
        let mut mutation = f.parent.begin_mutation().await?;
        // This initial Stop has no child group; the later phase is parent-only.
        let mut capture = TaskCaptureRequest::current();
        capture
            .required_keys
            .insert(TaskKey::new(TaskFamily::Task, &command_task.id));
        mutation.prepare_owner_task_request(&capture).await?;
        let captured = mutation.capture_tasks(&capture)?;
        let fact = captured
            .lookup(&TaskKey::new(TaskFamily::Task, &command_task.id))?
            .unwrap();
        let frozen = FrozenTaskStopIntent::prepare(
            fact,
            Some(&handle),
            f.parent.instance_owner_epoch(),
            None,
            f.parent.now(),
            &f.parent.inner.input_evidence_budget,
        )?;
        let command = TaskTransitionCommand {
            agent_id: "default".into(),
            task: frozen.task.clone(),
            agent_deletion: None,
            task_result_settlement: None,
            queue_entry: None,
            work_items: vec![],
            expected_wait_conditions: vec![],
            wait_conditions: vec![],
            agent_state: None,
            message_evidence: vec![],
            audit_events: vec![],
            index_changes: vec![],
            notify_scheduler: true,
            commit_on_idempotent: false,
            fault: None,
        };
        let original = PreparedInputTransition::task(
            mutation.input_evidence_scope(),
            "sparse-owner-initial".into(),
            frozen.task.updated_at,
            PreparedTaskTransition::new(command, Default::default(), false, None, None)?
                .with_stop_intent(frozen.intent.clone())?,
        )?;
        let binding = mutation.commit_initial_task_stop(original).await?.binding;
        drop((mutation, captured, frozen));
        let ledger = &f.parent.inner.input_evidence_budget;
        let source = f.parent.inner.runtime_db.transaction(|tx| {
            Ok(load_task_stop_by_sequence_tx(
                tx,
                &binding.scope,
                binding.effect_seq,
                request().byte_limit,
                ledger,
            )?)
        })?;
        let phase = PreparedInputTransition::task_stop_safe_progress(
            &source.records[0],
            "sparse-owner-phase".into(),
            f.parent.now(),
            TaskStopProgressAction::PhysicalAttention {
                reason: "no monitor".into(),
            },
            &request(),
            ledger,
        )?;
        // Create a genuinely cold State-only observation, then retain original accepted accounting.
        let state = f
            .parent
            .inner
            .mutation_owner
            .capture()?
            .unwrap()
            .state
            .clone();
        let owner = MutationOwner::standalone("default", binding.scope.identity_incarnation);
        let mut reservation = owner.try_reserve()?;
        reservation.begin("state-only".into())?;
        reservation.committed_prepared(state.clone(), None, None)?;
        reservation.publish(state)?;
        let accepted = match ledger.reserve_pending(&binding.scope, "unrelated-accepted", 17)? {
            super::super::super::input_evidence_budget::PendingAdmission::Reserved(ticket) => {
                ticket
            }
            _ => unreachable!(),
        };
        ledger.mark_attempt_started(&accepted)?;
        ledger.mark_accepted(&accepted)?;
        let witness = MutationWitness::input_mutation(
            "sparse-owner-phase".into(),
            OriginalInputMutationAttempt::new(phase.descriptor().clone()),
            vec![],
        )?;
        MutationReservation::begin_all_with_witness(
            std::slice::from_mut(&mut reservation),
            "sparse-owner-phase".into(),
            witness.clone(),
        )?;
        f.parent
            .inner
            .runtime_db
            .connection()?
            .execute("DELETE FROM agent_states WHERE agent_id='default'", [])?;
        f.parent
            .inner
            .runtime_db
            .transitions_in_scope(&binding.scope)
            .commit_prepared_transition(&phase)?;
        witness.finish_original_attempt();
        let pending = ledger.report()?.pending;
        super::super::super::input_recovery::resolve_input_recovery(
            &f.parent.inner.runtime_db,
            std::slice::from_mut(&mut reservation),
            ledger,
            &crate::context::ContextConfig::default(),
            &crate::config::RuntimeModelCatalog::default(),
        )?;
        let known = MutationReservation::validate_group_known(std::slice::from_ref(&reservation))?;
        assert!(matches!(&known[0],Some(KnownCommit::Metadata(facts)) if facts.state.is_none()));
        assert_eq!(ledger.report()?.pending, pending);
        MutationReservation::reconcile_all(std::slice::from_mut(&mut reservation), known)?;
        assert!(owner.capture()?.is_none());
        assert_eq!(ledger.report()?.pending, pending);
        Ok(())
    }
    #[tokio::test]
    async fn ordinary_task_receipt_is_not_sparse_stop_authority_and_child_budget_precedes_decode(
    ) -> Result<()> {
        let f = fixture().await?;
        let command = TaskTransitionCommand {
            agent_id: "default".into(),
            task: f.task.clone(),
            agent_deletion: None,
            task_result_settlement: None,
            queue_entry: None,
            work_items: vec![],
            expected_wait_conditions: vec![],
            wait_conditions: vec![],
            agent_state: None,
            message_evidence: vec![],
            audit_events: vec![],
            index_changes: vec![],
            notify_scheduler: false,
            commit_on_idempotent: false,
            fault: None,
        };
        let prepared = PreparedInputTransition::task(
            super::super::super::resident_input_evidence::InputEvidenceScope {
                agent_id: "default".into(),
                identity_incarnation: f.parent.inner.identity_incarnation,
            },
            "ordinary-not-stop".into(),
            f.task.updated_at,
            PreparedTaskTransition::new(command, Default::default(), false, None, None)?,
        )?;
        f.parent
            .inner
            .runtime_db
            .transitions_in_scope(&prepared.descriptor().scopes()[0])
            .commit_prepared_transition(&prepared)?;
        let attempt = OriginalInputMutationAttempt::new(prepared.descriptor().clone());
        attempt.finish_original_attempt();
        assert!(f
            .parent
            .inner
            .runtime_db
            .observe_task_stop_sparse_recovery(
                &attempt,
                &request(),
                &f.parent.inner.input_evidence_budget
            )?
            .is_none());
        f.parent.inner.runtime_db.connection()?.execute(
            "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
            rusqlite::params!["{bad".repeat(4096), f.target.scope.agent_id],
        )?;
        let mut limited = request();
        limited.byte_limit = 1024;
        let baseline = f
            .parent
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes;
        let error = f
            .parent
            .inner
            .runtime_db
            .transaction(|tx| {
                observe_task_stop_readonly_identity_tx(
                    tx,
                    &f.target,
                    &limited,
                    &f.parent.inner.input_evidence_budget,
                )
            })
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
            Some(
                crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                    "Stop readonly child identity"
                )
            )
        ));
        assert_eq!(
            f.parent
                .inner
                .input_evidence_budget
                .report()?
                .observed_bytes,
            baseline
        );
        Ok(())
    }
    #[tokio::test]
    async fn readonly_nonempty_queue_container_budget_precedes_clone_and_lives_with_saved_facts(
    ) -> Result<()> {
        let f = fixture().await?;
        let message = crate::types::MessageEnvelope::new(
            &f.target.scope.agent_id,
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "actual readonly child pending input".into(),
            },
        );
        f.child.enqueue(message).await?;
        let reservation = f.child.inner.mutation_owner.try_reserve()?;
        let root = reservation.capture_ready_snapshot()?.unwrap();
        let ledger = &f.child.inner.input_evidence_budget;
        let before = ledger.report()?.observed_bytes;
        let facts = Arc::new(
            f.child
                .prepare_external_input_facts(
                    &reservation,
                    root.state.clone(),
                    root.episodes.clone(),
                    root.input_evidence.clone(),
                )
                .await?,
        );
        assert_eq!(facts.queue.len(), 1);
        assert!(facts._readonly_queue_metadata.is_some());
        assert!(ledger.report()?.observed_bytes > before);
        let saved = facts.clone();
        drop(facts);
        drop(reservation);
        assert!(
            ledger.report()?.observed_bytes > before,
            "saved facts/witness must retain queue containers"
        );
        drop(saved);
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    }
}
