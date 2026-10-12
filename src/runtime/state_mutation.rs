//! Serialized private candidates and exact post-commit runtime installation.

use super::{
    mutation_owner::{IdentityMutationFacts, KnownCommit, MutationReservation, PublicationToken},
    *,
};
use crate::runtime_db::{persistence_certainty, PersistenceCertainty};

#[derive(Debug, thiserror::Error)]
#[error("agent mutation identity incarnation is no longer current: agent {agent_id}, expected incarnation {expected_incarnation}, actual incarnation {actual_incarnation}, status {status:?}")]
pub(super) struct RuntimeMutationIdentityRetired {
    agent_id: String,
    expected_incarnation: u64,
    actual_incarnation: u64,
    status: crate::types::AgentRegistryStatus,
}

#[derive(Default)]
pub(super) struct OwnedMutationTasks {
    closed: bool,
    drain: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl OwnedMutationTasks {
    pub(super) fn all_finished(&self) -> bool {
        self.handles.iter().all(|task| task.is_finished())
            && self
                .drain
                .try_lock()
                .is_ok_and(|tasks| tasks.iter().all(|task| task.is_finished()))
    }
}

#[derive(Debug, Clone)]
struct DeferredRunAbort {
    handle: CurrentRunAbortHandle,
    reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeMutationFacts {
    owner_epoch: u64,
    pub(crate) state: Arc<AgentState>,
    queue: RuntimeQueue,
    current_run_abort: Option<CurrentRunAbortHandle>,
    deferred_abort: Option<DeferredRunAbort>,
    effects: crate::runtime_db::transitions::PostCommitEffects,
}

pub(super) struct RuntimeMutation {
    runtime: RuntimeHandle,
    reservations: Vec<MutationReservation>,
    primary_index: usize,
    metadata: Vec<Option<Arc<IdentityMutationFacts>>>,
    candidate: Box<RuntimeAgent>,
    base: Box<RuntimeAgent>,
    deferred_abort: Option<DeferredRunAbort>,
    measurement_started: Option<std::time::Instant>,
}

impl std::ops::Deref for RuntimeMutation {
    type Target = RuntimeAgent;

    fn deref(&self) -> &Self::Target {
        &self.candidate
    }
}

impl std::ops::DerefMut for RuntimeMutation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.candidate
    }
}

impl RuntimeMutation {
    pub(super) fn defer_abort_current_run(&mut self, reason: impl Into<String>) {
        self.deferred_abort =
            self.candidate
                .current_run_abort
                .clone()
                .map(|handle| DeferredRunAbort {
                    handle,
                    reason: reason.into(),
                });
    }

    #[inline(never)]
    pub(super) fn persist_state<'a>(
        &'a mut self,
        _storage: &AppStorage,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.persist_state_inner())
    }

    async fn persist_state_inner(&mut self) -> Result<()> {
        self.validate_candidate_identity()?;
        if self.candidate.state == self.base.state {
            return self.install_transient().await;
        }
        let started = std::time::Instant::now();
        let event = AuditEvent::legacy(
            "agent_state_mutated",
            serde_json::json!({
                "agent_id": self.candidate.state.id,
            }),
        );
        let command = crate::runtime_db::transitions::AgentPostureTransitionCommand {
            agent_id: self.candidate.state.id.clone(),
            expected_incarnation: Some(self.runtime.inner.identity_incarnation),
            agent_state: crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(self.base.last_persisted_state.clone())),
                record: Box::new(self.candidate.state.clone()),
            },
            audit_events: vec![event.clone()],
            fault: self.runtime.take_transition_fault(),
        };
        let db = self.runtime.inner.runtime_db.clone();
        let result = self
            .commit_transition(event.id, || db.transitions().commit_agent_posture(&command))
            .await;
        crate::diagnostics::record_storage_persist_state(started.elapsed());
        let commit = result?;
        if !commit.applied && command.agent_state.record.as_ref() != &self.base.last_persisted_state
        {
            return Err(
                crate::runtime_db::RuntimeStateTransitionConflict::concurrent_mutation(
                    "agent_state",
                    &self.candidate.state.id,
                )
                .into(),
            );
        }
        self.runtime.apply_transition_commit(commit).await;
        Ok(())
    }

    // The closure adapts an existing typed repository command. It cannot
    // observe or mutate the live RuntimeAgent while SQL is in progress.
    #[inline(never)]
    pub(super) fn commit_transition<'a>(
        &'a mut self,
        operation_id: String,
        commit: impl FnOnce() -> Result<TransitionCommit> + Send + 'a,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<TransitionCommit>> + Send + 'a>>
    {
        // Keep transaction future construction out of nested runtime polls.
        Box::pin(self.commit_transition_inner(operation_id, commit))
    }

    async fn commit_transition_inner(
        &mut self,
        operation_id: String,
        commit: impl FnOnce() -> Result<TransitionCommit>,
    ) -> Result<TransitionCommit> {
        self.validate_candidate_identity()?;
        let keys = MutationReservation::begin_all(&mut self.reservations, operation_id)?;
        let mut committed = match commit() {
            Ok(committed) => committed,
            Err(error) => {
                if persistence_certainty(&error)
                    == Some(PersistenceCertainty::DefinitelyNotCommitted)
                {
                    MutationReservation::definitely_not_committed_all(&mut self.reservations)?;
                    self.candidate = self.base.clone();
                    self.deferred_abort = None;
                }
                return Err(error);
            }
        };
        if !committed.applied {
            MutationReservation::definitely_not_committed_all(&mut self.reservations)?;
            self.candidate = self.base.clone();
            self.deferred_abort = None;
            return Ok(committed);
        }
        if let Some(mutation) = committed.effects.agent_state.as_ref() {
            anyhow::ensure!(
                mutation.record.id == self.candidate.state.id,
                "committed runtime state identity mismatch"
            );
            self.candidate.state = mutation.record.as_ref().clone();
            self.candidate.last_persisted_state = self.candidate.state.clone();
        } else {
            anyhow::ensure!(
                self.candidate.state == self.base.state,
                "typed commit omitted a changed agent state"
            );
        }
        for message_id in &committed.effects.removed_queue_message_ids {
            self.candidate
                .queue
                .pop_next_matching(|message| &message.id == message_id);
        }
        for message in &committed.effects.queued_messages {
            if self
                .candidate
                .queue
                .peek_next_matching(|queued| queued.id == message.id)
                .is_none()
            {
                self.candidate.queue.push(message.clone());
            }
        }
        let facts = Arc::new(self.facts(committed.effects.clone()));
        // Retain all authoritative installation facts before the first await
        // after SQL. Cancellation leaves a known commit for the next owner call.
        self.record_committed_facts(facts.clone(), &committed.effects)?;
        if committed.effects.fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            bail!(
                "runtime mutation committed but publication requires reconciliation: {}",
                keys[self.primary_index].operation_id
            );
        }
        self.runtime.install_known_group(&self.reservations).await?;
        let observations = MutationReservation::publish_all_known(&mut self.reservations)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        strip_authoritative_effects(&mut committed.effects);
        if let Some(started) = self.measurement_started.take() {
            crate::diagnostics::io_probe::record_timing(
                crate::diagnostics::io_probe::TimingKind::CommitPublication,
                started.elapsed(),
            );
        }
        self.measurement_started = crate::diagnostics::io_probe::start_timer();
        Ok(committed)
    }

    pub(super) async fn install_transient(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate.state == self.base.state
                && self.candidate.last_persisted_state == self.base.last_persisted_state,
            "transient installation cannot change durable agent state"
        );
        self.install_durable_readback().await
    }

    pub(super) async fn install_durable_readback(&mut self) -> Result<()> {
        self.validate_candidate_identity()?;
        anyhow::ensure!(
            self.candidate.state == self.candidate.last_persisted_state,
            "durable readback must install one exact persisted state"
        );
        MutationReservation::begin_all(&mut self.reservations, crate::ids::audit_event_id())?;
        let facts = Arc::new(self.facts(Default::default()));
        self.record_committed_facts(facts.clone(), &Default::default())?;
        self.runtime.install_known_group(&self.reservations).await?;
        let observations = MutationReservation::publish_all_known(&mut self.reservations)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        Ok(())
    }

    // A caller that owns a committed operation may finish its saved installation
    // without releasing the reservation or guessing the result of unknown SQL.
    pub(super) async fn reconcile_known_commit(&mut self) -> Result<bool> {
        let known = match MutationReservation::validate_group_known(&self.reservations) {
            Ok(known) => known,
            Err(mutation_owner::MutationOwnerError::NeedsReconcile(_)) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if known.iter().all(Option::is_none) {
            return Ok(false);
        }
        self.runtime
            .reconcile_reserved_mutations(&mut self.reservations)
            .await?;
        for (metadata, saved) in self.metadata.iter_mut().zip(known) {
            if let Some(KnownCommit::Metadata(facts)) = saved {
                *metadata = Some(facts);
            }
        }
        self.candidate = Box::new(self.runtime.inner.agent.lock().await.clone());
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        Ok(true)
    }

    fn validate_candidate_identity(&self) -> Result<()> {
        let agent_id = self.runtime.inner.mutation_owner.agent_id();
        anyhow::ensure!(
            self.candidate.state.id == agent_id && self.base.state.id == agent_id,
            "cannot write agent state for `{}` through storage scoped to `{agent_id}`",
            self.candidate.state.id
        );
        Ok(())
    }

    fn record_committed_facts(
        &mut self,
        facts: Arc<RuntimeMutationFacts>,
        effects: &crate::runtime_db::transitions::PostCommitEffects,
    ) -> Result<()> {
        let known = self
            .reservations
            .iter()
            .enumerate()
            .map(|(index, reservation)| {
                if index == self.primary_index {
                    return Ok(KnownCommit::State {
                        state: facts.state.clone(),
                        runtime: Some(facts.clone()),
                    });
                }
                let mut metadata = self.metadata[index]
                    .as_ref()
                    .ok_or_else(|| anyhow!("related runtime metadata snapshot is missing"))?
                    .as_ref()
                    .clone();
                if let Some(identity) = effects
                    .agent_identities
                    .iter()
                    .find(|identity| identity.agent_id == reservation.agent_id())
                {
                    anyhow::ensure!(
                        identity.incarnation == reservation.identity_incarnation(),
                        "committed related identity incarnation mismatch"
                    );
                    metadata.identity = Some(Arc::new(identity.clone()));
                }
                Ok(KnownCommit::Metadata(Arc::new(metadata)))
            })
            .collect::<Result<Vec<_>>>()?;
        let metadata = known
            .iter()
            .map(|known| match known {
                KnownCommit::Metadata(facts) => Some(facts.clone()),
                KnownCommit::State { .. } => None,
            })
            .collect();
        MutationReservation::committed_all(&mut self.reservations, known)?;
        self.metadata = metadata;
        Ok(())
    }

    pub(super) async fn restore_bootstrap_replay_message(
        &mut self,
        storage: &AppStorage,
        message: &MessageEnvelope,
    ) -> Result<()> {
        if self
            .candidate
            .queue
            .peek_next_matching(|queued| queued.id == message.id)
            .is_none()
        {
            self.candidate.queue.push(message.clone());
        }
        let queued_messages = self.candidate.queue.len();
        self.candidate.state.pending = queued_messages;
        scheduler_executor::apply_bootstrap_recovered_projection(
            &mut self.candidate.state,
            scheduler_executor::BootstrapRecoveryFacts { queued_messages },
        );
        self.persist_state(storage).await
    }

    fn facts(
        &self,
        effects: crate::runtime_db::transitions::PostCommitEffects,
    ) -> RuntimeMutationFacts {
        RuntimeMutationFacts {
            owner_epoch: self.reservations[self.primary_index].owner_epoch(),
            state: Arc::new(self.candidate.state.clone()),
            queue: self.candidate.queue.clone(),
            current_run_abort: self.candidate.current_run_abort.clone(),
            deferred_abort: self.deferred_abort.clone(),
            effects,
        }
    }
}

fn strip_authoritative_effects(effects: &mut crate::runtime_db::transitions::PostCommitEffects) {
    effects.agent_state = None;
    effects.queued_messages.clear();
    effects.removed_queue_message_ids.clear();
    effects.work_items.clear();
    effects.tasks.clear();
    effects.timers.clear();
    effects.external_triggers.clear();
    effects.agent_identities.clear();
}

impl RuntimeHandle {
    pub(crate) fn spawn_owned_mutation(
        &self,
        mutation: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let mut tasks = self
            .inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned");
        if tasks.closed {
            return Err(super::execution_admission::closed_error());
        }
        tasks.handles.retain(|task| !task.is_finished());
        // Registration and closing share one gate. Shutdown cannot miss a task
        // spawned immediately after taking the previous handles to drain.
        tasks.handles.push(tokio::spawn(mutation));
        Ok(())
    }

    pub(super) fn close_mutation_admission(&self) {
        self.inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned")
            .closed = true;
    }

    pub(crate) async fn join_owned_mutations(&self) -> Result<()> {
        let drain = {
            let mut tasks = self
                .inner
                .mutation_tasks
                .lock()
                .expect("mutation tasks poisoned");
            tasks.closed = true;
            tasks.drain.clone()
        };
        let mut draining = drain.lock().await;
        draining.extend(std::mem::take(
            &mut self
                .inner
                .mutation_tasks
                .lock()
                .expect("mutation tasks poisoned")
                .handles,
        ));
        let mut failure = None;
        while let Some(task) = draining.last_mut() {
            // The supervisor retains the handle across await. Cancelling this
            // drainer leaves it for the next drainer rather than detaching it.
            let result = task.await;
            draining.pop();
            if let Err(error) = result {
                failure.get_or_insert_with(|| anyhow::Error::new(error));
            }
        }
        drop(draining);
        let reconciled = self.reconcile_posture_observation().await;
        failure.map_or(reconciled, Err)
    }

    #[inline(never)]
    pub(super) fn begin_mutation(
        &self,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<RuntimeMutation>> + Send + '_>>
    {
        Box::pin(self.begin_mutation_with_owners(vec![self.inner.mutation_owner.clone()]))
    }

    pub(super) fn validate_mutation_identity(&self) -> Result<()> {
        if let Some(identity) = self
            .inner
            .runtime_db
            .agent_identities()
            .latest(self.inner.mutation_owner.agent_id())?
        {
            if identity.incarnation != self.inner.identity_incarnation
                || identity.status == crate::types::AgentRegistryStatus::Deleted
            {
                return Err(RuntimeMutationIdentityRetired {
                    agent_id: identity.agent_id,
                    expected_incarnation: self.inner.identity_incarnation,
                    actual_incarnation: identity.incarnation,
                    status: identity.status,
                }
                .into());
            }
        }
        Ok(())
    }

    #[inline(never)]
    pub(super) fn begin_mutation_with_agent_deletion<'a>(
        &'a self,
        deletion: Option<&'a crate::runtime_db::repositories::AgentDeletionRequest>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<RuntimeMutation>> + Send + 'a>>
    {
        Box::pin(self.begin_mutation_with_agent_deletion_inner(deletion))
    }

    async fn begin_mutation_with_agent_deletion_inner(
        &self,
        deletion: Option<&crate::runtime_db::repositories::AgentDeletionRequest>,
    ) -> Result<RuntimeMutation> {
        let Some(deletion) = deletion else {
            return self.begin_mutation().await;
        };
        let identity = self
            .inner
            .runtime_db
            .agent_identities()
            .latest(&deletion.agent_id)?;
        let Some(identity) = identity else {
            return self.begin_mutation().await;
        };
        let owner = if identity.agent_id == self.inner.mutation_owner.agent_id()
            && identity.incarnation == self.inner.identity_incarnation
        {
            self.inner.mutation_owner.clone()
        } else {
            match self.inner.host_bridge.as_ref() {
                Some(bridge) => {
                    bridge.mutation_owner_for(&identity.agent_id, identity.incarnation)?
                }
                None => self
                    .inner
                    .standalone_related_mutation_owners
                    .current_owner(&identity.agent_id, identity.incarnation)?,
            }
        };
        self.begin_mutation_with_owners(vec![self.inner.mutation_owner.clone(), owner])
            .await
    }

    async fn begin_mutation_with_owners(
        &self,
        owners: Vec<mutation_owner::MutationOwner>,
    ) -> Result<RuntimeMutation> {
        let measurement_started = crate::diagnostics::io_probe::start_timer();
        // reserve_all also acquires every member of a pending recovery group.
        let requested = owners.clone();
        let mut reservations = mutation_owner::MutationOwner::reserve_all(owners).await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        // Recovery can add scopes from an older operation. After atomic recovery
        // the new command owns only its explicit write set.
        reservations.retain(|reservation| {
            requested
                .iter()
                .any(|owner| reservation.validate_owner(owner).is_ok())
        });
        let primary_index = reservations
            .iter()
            .position(|reservation| {
                reservation
                    .validate_owner(&self.inner.mutation_owner)
                    .is_ok()
            })
            .ok_or_else(|| anyhow!("runtime mutation reservation is missing"))?;
        if let Some(observation) = self.inner.mutation_owner.capture()? {
            anyhow::ensure!(
                observation.state.id == self.inner.mutation_owner.agent_id(),
                "published runtime state identity mismatch"
            );
        }
        self.validate_mutation_identity()?;
        let metadata = reservations
            .iter()
            .enumerate()
            .map(|(index, reservation)| {
                if index == primary_index {
                    return Ok(None);
                }
                let identity = self
                    .inner
                    .runtime_db
                    .agent_identities()
                    .latest(reservation.agent_id())?;
                if let Some(identity) = identity.as_ref() {
                    anyhow::ensure!(
                        identity.incarnation == reservation.identity_incarnation(),
                        "related mutation identity incarnation is no longer current"
                    );
                }
                let state = self
                    .inner
                    .runtime_db
                    .agent_states()
                    .latest(reservation.agent_id())?;
                Ok(Some(Arc::new(IdentityMutationFacts {
                    identity: identity.map(Arc::new),
                    state: state.map(Arc::new),
                })))
            })
            .collect::<Result<Vec<_>>>()?;
        let candidate = Box::new(self.inner.agent.lock().await.clone());
        Ok(RuntimeMutation {
            runtime: self.clone(),
            reservations,
            primary_index,
            metadata,
            base: candidate.clone(),
            candidate,
            deferred_abort: None,
            measurement_started,
        })
    }

    async fn install_mutation_facts(&self, facts: &RuntimeMutationFacts) -> Result<()> {
        // The reservation remains held; state/cache readers can only see the
        // old publication or the complete new publication after this block.
        {
            let mut agent = self.inner.agent.lock().await;
            agent.state = facts.state.as_ref().clone();
            agent.last_persisted_state = facts.state.as_ref().clone();
            agent.queue = facts.queue.clone();
            agent.current_run_abort = facts.current_run_abort.clone();
            if let Some(abort) = &facts.deferred_abort {
                if let Ok(mut reason) = abort.handle.reason.lock() {
                    *reason = abort.reason.clone();
                }
                abort.handle.token.cancel();
            }
        }
        let mut cache = self.inner.projection_cache.lock().await;
        for record in &facts.effects.work_items {
            cache.upsert_work_item(record.clone());
        }
        for record in &facts.effects.tasks {
            cache.upsert_task(record.clone());
        }
        for record in &facts.effects.timers {
            cache.upsert_timer(record.clone());
        }
        for record in &facts.effects.external_triggers {
            cache.upsert_external_trigger(record.clone());
        }
        drop(cache);
        if let Some(bridge) = self.inner.host_bridge.as_ref() {
            for identity in facts
                .effects
                .agent_identities
                .iter()
                .filter(|identity| identity.agent_id == self.inner.mutation_owner.agent_id())
            {
                bridge
                    .install_mutation_metadata(
                        &identity.agent_id,
                        self.inner.identity_incarnation,
                        &IdentityMutationFacts {
                            identity: Some(Arc::new(identity.clone())),
                            state: Some(facts.state.clone()),
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn reconcile_posture_observation(&self) -> Result<()> {
        let mut reservations =
            mutation_owner::MutationOwner::reserve_all(vec![self.inner.mutation_owner.clone()])
                .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        Ok(())
    }

    async fn install_known_group(&self, reservations: &[MutationReservation]) -> Result<()> {
        let known = MutationReservation::validate_group_known(reservations)?;
        for (reservation, saved) in reservations.iter().zip(&known) {
            let Some(saved) = saved else {
                continue;
            };
            if let KnownCommit::Metadata(metadata) = saved {
                if let Some(bridge) = self.inner.host_bridge.as_ref() {
                    bridge
                        .install_mutation_metadata(
                            reservation.agent_id(),
                            reservation.identity_incarnation(),
                            metadata,
                        )
                        .await?;
                }
            }
            let runtime = if reservation
                .validate_owner(&self.inner.mutation_owner)
                .is_ok()
            {
                Some(self.clone())
            } else {
                match self.inner.host_bridge.as_ref() {
                    Some(bridge) => bridge.runtime_for_mutation(reservation).await?,
                    None => None,
                }
            };
            if let Some(runtime) = runtime {
                runtime.install_saved_mutation(reservation, saved).await?;
            }
        }
        Ok(())
    }

    // The slice must contain the whole pending group. No member becomes readable
    // until every saved installation has completed and all gates validate.
    pub(crate) async fn reconcile_reserved_mutations(
        &self,
        reservations: &mut [MutationReservation],
    ) -> Result<bool> {
        let known = MutationReservation::validate_group_known(reservations).map_err(|error| {
            match error {
                mutation_owner::MutationOwnerError::NeedsReconcile(key) => anyhow!(
                    "runtime commit outcome is unknown; no durable receipt proves the original operation {}", key.operation_id),
                other => other.into(),
            }
        })?;
        if known.iter().all(Option::is_none) {
            return Ok(false);
        }
        let mut notifications = Vec::new();
        for (reservation, saved) in reservations.iter().zip(&known) {
            let Some(saved) = saved else {
                continue;
            };
            if let Some(recovery) = reservation.recovery()? {
                tracing::warn!(agent_id = %recovery.key.agent_id,
                    operation_id = %recovery.key.operation_id, phase = ?recovery.phase,
                    original_outcome_known = true,
                    "restoring committed runtime mutation group without replay");
            }
            if let KnownCommit::Metadata(metadata) = saved {
                if let Some(bridge) = self.inner.host_bridge.as_ref() {
                    bridge
                        .install_mutation_metadata(
                            reservation.agent_id(),
                            reservation.identity_incarnation(),
                            metadata,
                        )
                        .await?;
                }
            }
            let runtime = if reservation
                .validate_owner(&self.inner.mutation_owner)
                .is_ok()
            {
                Some(self.clone())
            } else {
                match self.inner.host_bridge.as_ref() {
                    Some(bridge) => bridge.runtime_for_mutation(reservation).await?,
                    None => None,
                }
            };
            if let Some(runtime) = runtime {
                if let Some(effects) = runtime.install_saved_mutation(reservation, saved).await? {
                    notifications.push((runtime, effects));
                }
            }
        }
        let observations = MutationReservation::reconcile_all(reservations, known)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        for (runtime, effects) in notifications {
            runtime.reissue_mutation_notifications(effects).await;
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) async fn prepare_known_host_installation_fixture(
        &self,
    ) -> Result<(AgentState, CancellationToken, String)> {
        let agent_id = self.agent_state().await?.id;
        let message = self
            .enqueue(MessageEnvelope::new(
                agent_id.clone(),
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: "known Host installation fixture".into(),
                },
            ))
            .await?;
        let token = CancellationToken::new();
        {
            let mut mutation = self.begin_mutation().await?;
            mutation.state.status = AgentStatus::AwakeRunning;
            mutation.state.current_run_id = Some("host-installation-fixture".into());
            mutation.current_run_abort = Some(CurrentRunAbortHandle {
                run_id: "host-installation-fixture".into(),
                token: token.clone(),
                reason: Arc::new(StdMutex::new("fixture-active".into())),
            });
            mutation.persist_state(&self.inner.storage).await?;
        }
        let mut mutation = self.begin_mutation().await?;
        mutation.defer_abort_current_run("fixture-committed-abort");
        mutation.current_run_abort = None;
        mutation.queue.clear();
        mutation.state.pending = 0;
        mutation.state.status = AgentStatus::AwakeIdle;
        mutation.state.current_run_id = None;
        let expected_state = mutation.state.clone();
        let expected_queue = self
            .inner
            .runtime_db
            .queue_entries()
            .latest(&message.id)?
            .context("fixture queue entry is missing")?;
        let mut aborted_queue = expected_queue.clone();
        aborted_queue.status = QueueEntryStatus::Aborted;
        aborted_queue.updated_at = self.now();
        let event = AuditEvent::legacy("host_installation_fixture", serde_json::json!({}));
        let command = crate::runtime_db::transitions::QueueTransitionCommand {
            agent_id,
            operation: crate::runtime_db::transitions::QueueOperation::Discard,
            mutation: crate::runtime_db::transitions::QueueMutation::Upsert(aborted_queue),
            scheduler_claim_work_item: None,
            agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(mutation.last_persisted_state.clone())),
                record: Box::new(expected_state.clone()),
            }),
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: vec![event.clone()],
            notify_scheduler: false,
            fault: Some(TransitionFaultPoint::BeforeCacheUpdate),
            brief_evidence: Vec::new(),
        };
        let result = mutation
            .commit_transition(event.id, || {
                self.inner.runtime_db.transitions().commit_queue(&command)
            })
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => bail!("fixture must leave publication pending"),
        };
        anyhow::ensure!(
            error
                .to_string()
                .contains("committed but publication requires reconciliation"),
            "fixture must reach the known publication fault: {error:#}"
        );
        let recovery = mutation.reservations[mutation.primary_index]
            .recovery()?
            .context("fixture recovery facts are missing")?;
        anyhow::ensure!(
            recovery.committed_state.as_deref() == Some(&expected_state)
                && recovery.committed_runtime.is_some(),
            "fixture must retain exact known runtime facts"
        );
        Ok((expected_state, token, message.id))
    }

    #[cfg(test)]
    pub(crate) async fn host_installation_snapshot(&self) -> (AgentState, usize, bool) {
        let agent = self.inner.agent.lock().await;
        (
            agent.state.clone(),
            agent.queue.len(),
            agent.current_run_abort.is_none(),
        )
    }

    pub(crate) fn instance_agent_id(&self) -> &str {
        self.inner.mutation_owner.agent_id()
    }

    pub(crate) fn owns_mutation_reservation(&self, reservation: &MutationReservation) -> bool {
        reservation
            .validate_owner(&self.inner.mutation_owner)
            .is_ok()
    }

    pub(crate) async fn install_saved_mutation(
        &self,
        reservation: &MutationReservation,
        known: &KnownCommit,
    ) -> Result<Option<crate::runtime_db::transitions::PostCommitEffects>> {
        // Old-runtime handles and abort tokens do not cross an owner replacement.
        if reservation
            .validate_owner(&self.inner.mutation_owner)
            .is_err()
        {
            return Ok(None);
        }
        match known {
            KnownCommit::State {
                state,
                runtime: facts,
            } => {
                if let Some(facts) = facts
                    .as_ref()
                    .filter(|facts| facts.owner_epoch == reservation.owner_epoch())
                {
                    self.install_mutation_facts(facts).await?;
                    return Ok(Some(facts.effects.clone()));
                }
                self.install_exact_durable_readback(state).await?;
            }
            KnownCommit::Metadata(metadata) => {
                if let Some(state) = &metadata.state {
                    self.install_exact_durable_readback(state).await?;
                }
            }
        }
        Ok(None)
    }

    pub(crate) async fn reissue_mutation_notifications(
        &self,
        mut effects: crate::runtime_db::transitions::PostCommitEffects,
    ) {
        strip_authoritative_effects(&mut effects);
        if effects.fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            effects.fault = None;
        }
        // Reissue only rediscoverable notifications, never business SQL.
        self.apply_transition_commit(TransitionCommit {
            applied: true,
            effects,
            delivery_receipt: None,
        })
        .await;
    }

    async fn install_exact_durable_readback(&self, state: &Arc<AgentState>) -> Result<()> {
        let mut agent = self.inner.agent.lock().await;
        anyhow::ensure!(
            agent.state == agent.last_persisted_state,
            "runtime recovery cannot discard unpersisted state"
        );
        anyhow::ensure!(
            agent
                .current_run_abort
                .as_ref()
                .is_none_or(|handle| { state.current_run_id.as_ref() == Some(&handle.run_id) }),
            "runtime recovery conflicts with a live run handle"
        );
        agent.state = state.as_ref().clone();
        agent.last_persisted_state = state.as_ref().clone();
        Ok(())
    }
}

fn trace_publication(token: PublicationToken) {
    tracing::trace!(
        identity_incarnation = token.identity_incarnation,
        owner_epoch = token.owner_epoch,
        publication_generation = token.generation,
        "published committed posture observation"
    );
}

#[cfg(test)]
#[path = "state_mutation_tests.rs"]
mod tests;
